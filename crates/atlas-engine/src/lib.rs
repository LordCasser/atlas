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
//! use atlas_engine::{Engine, SourcePath};
//!
//! let engine = Engine::open_in_memory()?;
//! let source = atlas_engine::decode_source(b"function f() {}");
//! engine.extract_file_with_mode(
//!     &SourcePath::from_relative("test.ts"),
//!     &source,
//!     atlas_engine::Language::TypeScript,
//!     atlas_engine::ExtractionMode::Full,
//! )?;
//! ```
//!
//! # Feature flags
//!
//! All 14 languages are compiled by default. Individual languages can be
//! disabled by opting out of default features (e.g. `--no-default-features`).

use std::path::Path;
use std::sync::Arc;

// ── Internal modules ──────────────────────────────────────────────────────

mod closure_planner;
/// Focus-driven incremental analysis (control plane + materialize).
pub mod focus;
mod index_precision;
/// Investigation context: MCP-session-scoped analysis focus for Focus job prioritization.
pub mod investigation;
mod lazy_budget;
/// AnswerQuality tier computation for Focus materialize transparency.
pub mod precision;
/// Scoped search service: shared search orchestration with lazy structural fallback.
pub mod scoped_search;
mod source_extractor;
/// Unified symbol resolution with fault-tolerant scoring.
pub mod symbol_selector;

// ── Stable Public API ─────────────────────────────────────────────────────
// These items form the intended, stable contract of the Atlas engine.
// External consumers should rely only on these. During the current prototype
// phase, intentional breaking changes are documented rather than shimmed.

/// Extraction: entry points and mode control.
pub use extraction::{
    ExtractionMode, LanguageFrontend, create_frontend, extract_file_with_mode, parse_analysis_mode,
};

/// Sync layer: core indexing pipeline and progress protocol.
pub use filesync::{
    ChangedFiles, FileLock, IndexLockHeld, IndexPipeline, IndexPipelineOptions, IndexPipelineStats,
    NoopSink, PhaseName, ProgressEvent, ProgressSink, SyncEngine, SyncStats,
    index_pipeline::PipelinePhaseTiming,
};

/// Database store and schema version.
pub use db::{CURRENT_SCHEMA_VERSION, DiscoveredFile, Store};

/// Graph layer: builder, query engine, snapshots, annotation materialization.
pub use graph::{
    CallGraphView, CompositePathScore, ForwardFrontier, FrontierNode, GraphBuilder,
    GraphBuilderStats, GraphEngine, GraphPath, GraphSnapshot, NodeIx, PathBreakpoint,
    PathBreakpointKind, PathEdge, PathEdgeDirection, RankedPath, Subgraph, TraversalConfig,
    TraversalDirection, materialize_annotations,
};

/// Resolution layer: reference resolver and path aliases.
pub use resolution::{PathAliasConfig, PathAliasResolver, ReferenceResolver, ResolutionStats};

/// Analysis: trace engine (low-level; does not trigger Focus materialize).
///
/// [`RawTraceEngine`] only reads existing dataflow facts. Callers must ensure
/// facts via [`FocusMaterialize`] / [`LazyDataflowService::ensure_for_position`],
/// or use high-level [`Engine::trace_variable`] which wraps both.
pub use analysis::trace::TraceEngine as RawTraceEngine;
/// Analysis: trace query responses.
pub use analysis::trace::TraceQueryResponse;

/// Context layer: AI context builder and its public view types.
pub use context::{CalleeDetail, CallerDetail, ContextBuilder, ContextView};

/// Search layer: FTS5 + fuzzy search engine and query parser.
pub use search::query_parser::{ParsedQuery, parse_query};
pub use search::{SearchEngine, SearchOptions, SearchResult};

/// Scoped search: shared orchestration for MCP/TUI search with lazy structural fallback.
pub use scoped_search::{
    ScopedSearchRequest, ScopedSearchResponse, ScopedSearchService, SearchAnalysis, SearchCoverage,
    seed_file_inventory_from_scope,
};

/// Focus materialize: on-demand structural + dataflow under the Focus solution.
///
/// Construct via [`FocusMaterialize::open`] only. Mechanism service types
/// (`LazyDataflowService`, `LazyStructuralService`) are ensure APIs whose names
/// mean CS deferred evaluation — not a separate product door or AccessStrategy.
pub use focus::{
    CandidateProvider, DefaultCandidateProvider, EnsureStructuralResult, FocusMaterialize,
    LazyDataflowService, LazyStructuralService, rebuild_structural_for_file,
};

/// Source extraction: AST-based symbol source retrieval.
pub use source_extractor::SourceExtractor;

/// Investigation context types: focus, related symbols/files, desired capabilities.
pub use investigation::{Investigation, InvestigationFocus};

/// Focus-driven incremental analysis: closure engine.
pub use focus::engine::ClosureEngine;
/// Focus-driven incremental analysis: query intent (MCP tool request).
pub use focus::query::{QueryIntent, QueryNeed};
/// Focus-driven incremental analysis: runtime entry point.
pub use focus::runtime::{AccessStrategy, FocusResult, FocusRuntime};
/// Focus-driven incremental analysis: priority scheduler.
pub use focus::scheduler::{FocusPriority, FocusScheduler};
/// Focus-driven incremental analysis: core types.
pub use focus::types::{
    ClosureStrategy, FocusClosure, FocusSeed, FocusWindow, INTERACTIVE_QUERY_BUDGET_MS,
    WindowBudget,
};
/// Focus-driven incremental analysis: visibility filter registry.
pub use focus::visibility_filter::VisibilityFilterRegistry;

/// Closure planner: include-root for dependency-closure extraction.
pub use closure_planner::IncludeRoot;

/// Index precision: guards and queries for extraction mode stability.
pub use index_precision::{
    guard_against_precision_downgrade, has_finalized_repo_cache_for, is_rich_catalog_tier,
};

/// Workspace abstractions.
pub use workspace::{ProjectRoot, SourcePath, SourceText, Workspace, decode_source, read_source};

/// Progress protocol (for CLI TUI integration).
pub use types::progress;
/// All core IR types (SymbolDef, ReferenceUse, FileFacts, etc.).
pub use types::*;

// ── Internal / Prelude ───────────────────────────────────────────────────
// These items are exported for convenience of workspace-internal crates
// (CLI, MCP, TUI).  They may change between minor versions without
// notice.  External integrators should not rely on them.

/// Analysis: lifecycle and branch diff engines (full crate re-export).
pub use analysis;
/// Analysis: trace module (for qualified access to trace sub-items).
pub use analysis::trace;

/// Dossier: Symbol Dossier builder for atlas_explore tool.
pub use dossier;

/// Domain rules: language-agnostic rule engine (aliased).
pub use domain_rules as rule_engine;

// ─── Engine ────────────────────────────────────────────────────────────────

/// High-level Atlas engine wrapping the full extraction → resolution → trace pipeline.
///
/// # Lifecycle
///
/// ```text
///   open() / open_in_memory()
///     └── extract_file_with_mode()# parse + extract FileFacts
///     └── language_capability()   # query language profiles
///     └── trace_variable()        # backward dataflow trace
///     └── trace_callers()         # reverse call-graph exploration
///     └── trace_point()           # position-to-context resolution
/// ```
pub struct Engine {
    store: Arc<Store>,
    /// Focus-owned on-demand structural + dataflow materialize stack.
    materialize: FocusMaterialize,
    trace: analysis::trace::TraceEngine,
}

impl Engine {
    // ── Constructors ───────────────────────────────────────────────────

    fn from_parts(
        store: Arc<Store>,
        materialize: FocusMaterialize,
        trace: analysis::trace::TraceEngine,
    ) -> Self {
        Self {
            store,
            materialize,
            trace,
        }
    }

    /// Open an existing database file.
    ///
    /// The database must have been created by `atlas init` or a prior run.
    /// The schema is NOT initialized here — that is the CLI's responsibility.
    ///
    /// **Limitation**: Opens without a project root.  Focus materialize that
    /// reads source files from disk requires the DB to be opened from the
    /// project root directory (sources are stored as relative paths).  Use
    /// [`Engine::open_with_root`] when needed.
    pub fn open(db_path: &Path) -> anyhow::Result<Self> {
        let store = Arc::new(Store::open_db(db_path)?);
        let materialize = FocusMaterialize::open(store.clone(), None);
        let trace = analysis::trace::TraceEngine::new(store.clone());
        Ok(Self::from_parts(store, materialize, trace))
    }

    /// Open a database file with a project root for snippet extraction.
    pub fn open_with_root(db_path: &Path, project_root: &Path) -> anyhow::Result<Self> {
        let store = Arc::new(Store::open_db(db_path)?);
        let materialize = FocusMaterialize::open(store.clone(), Some(project_root.to_path_buf()));
        let trace =
            analysis::trace::TraceEngine::new_with_root(store.clone(), project_root.to_path_buf());
        Ok(Self::from_parts(store, materialize, trace))
    }

    /// Open an in-memory database (for testing).
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let store = Store::open_in_memory()?;
        store.init_schema()?;
        let store = Arc::new(store);
        let materialize = FocusMaterialize::open(store.clone(), None);
        let trace = analysis::trace::TraceEngine::new(store.clone());
        Ok(Self::from_parts(store, materialize, trace))
    }

    /// Construct an Engine from an existing [`Arc<Store>`].
    ///
    /// Opens a **new** [`FocusMaterialize`] for this store (own Arc stack).
    /// Correct for standalone CLI / TUI jobs that own the process boundary.
    ///
    /// **MCP / multi-runtime processes:** use [`Engine::from_materialize`] so
    /// Engine, FocusRuntime, and AnalysisRuntime share one stack identity
    /// (`FocusMaterialize::same_stack_as`). Calling `from_store` beside an
    /// existing FocusRuntime creates a second materialize configuration.
    pub fn from_store(store: Arc<Store>, project_root: Option<&std::path::Path>) -> Self {
        let materialize =
            FocusMaterialize::open(store.clone(), project_root.map(|p| p.to_path_buf()));
        Self::from_materialize(store, materialize, project_root)
    }

    /// Construct an Engine that shares an existing [`FocusMaterialize`] stack.
    ///
    /// Preferred when the process already has one Focus materialize configuration
    /// (e.g. MCP ActiveProject).
    pub fn from_materialize(
        store: Arc<Store>,
        materialize: FocusMaterialize,
        project_root: Option<&std::path::Path>,
    ) -> Self {
        let trace = if let Some(root) = project_root {
            analysis::trace::TraceEngine::new_with_root(store.clone(), root.to_path_buf())
        } else {
            analysis::trace::TraceEngine::new(store.clone())
        };
        Self::from_parts(store, materialize, trace)
    }

    /// Access the underlying database store.
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Access the underlying trace engine.
    pub fn trace_engine(&self) -> &analysis::trace::TraceEngine {
        &self.trace
    }

    /// Focus materialize stack (structural + dataflow ensure).
    pub fn materialize(&self) -> &FocusMaterialize {
        &self.materialize
    }

    /// Structural ensure service (Focus materialize).
    pub fn focus_structural(&self) -> &LazyStructuralService {
        self.materialize.structural()
    }

    // ── Extraction ─────────────────────────────────────────────────────

    /// Extract facts from a decoded source file with explicit mode control.
    ///
    /// [`SourceText::file_hash`] preserves the raw file identity while
    /// [`SourceText::text`] provides the decoded UTF-8 parser input.
    /// The path is project-relative because Atlas file identity is path-based.
    /// Blob/version-oriented consumers such as Atlas Corpus must call the
    /// lower-level extraction API with their own [`FileId`].
    pub fn extract_file_with_mode(
        &self,
        path: &SourcePath,
        source: &SourceText,
        language: Language,
        mode: extraction::ExtractionMode,
    ) -> anyhow::Result<FileFacts> {
        let frontend = extraction::create_frontend(language)
            .ok_or_else(|| anyhow::anyhow!("Language frontend not available for {language:?}"))?;
        let file_id = FileId::generate(path.as_str());
        let parser_path = Path::new(path.as_str());
        let facts = extraction::extract_file_with_mode(
            &frontend,
            file_id,
            parser_path,
            &source.text,
            &source.file_hash,
            mode,
            &(),
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
    /// Requires `DataflowLocal` capability for the language.  If the language
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
            .map(|c| c.features.local_dataflow.is_supported())
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
        let mut lazy_summary: Option<LazySummary>;
        match self
            .materialize
            .dataflow()
            .ensure_for_position_with_depth(file_id, line, column, max_depth, None)
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
                    quality: window.quality.clone(),
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
                    quality: None,
                });
                lazy_diagnostics.push(
                    TraceDiagnostic::warning(&format!("Lazy dataflow build failed: {e}"))
                        .with_code("lazy_dataflow_build_failed"),
                );
            }
        }

        let is_arkts = cap
            .as_ref()
            .is_some_and(|profile| profile.language == Language::ArkTS.as_str());
        if is_arkts {
            match analysis::trace::virtual_edges::arkts_state_writer_functions_for_file(
                file_id,
                self.store.as_ref(),
            ) {
                Ok(writer_functions) => {
                    let mut state_window_truncated = false;
                    let mut state_window_pending = false;
                    for function_id in writer_functions {
                        match self.materialize.dataflow().ensure_for_function_with_depth(
                            &function_id,
                            max_depth,
                            None,
                        ) {
                            Ok(window) => {
                                state_window_truncated |= window.truncated;
                                if window.units_pending > 0 {
                                    partial = true;
                                    state_window_pending = true;
                                }
                                if let Some(summary) = lazy_summary.as_mut() {
                                    summary.units_built += window.units_built;
                                    summary.units_cached += window.units_cached;
                                    summary.units_pending += window.units_pending;
                                    summary.truncated |= window.truncated;
                                    for job_id in window.pending_job_ids {
                                        if !summary.pending_job_ids.contains(&job_id) {
                                            summary.pending_job_ids.push(job_id);
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                partial = true;
                                lazy_diagnostics.push(
                                    TraceDiagnostic::warning(&format!(
                                        "AppStorage writer dataflow build failed: {error}"
                                    ))
                                    .with_code("state_source_dataflow_build_failed"),
                                );
                            }
                        }
                    }
                    if state_window_truncated {
                        partial = true;
                        lazy_diagnostics.push(
                            TraceDiagnostic::warning(
                                "AppStorage writer dataflow reached its internal budget. Result is partial.",
                            )
                            .with_code("state_source_dataflow_budget_exceeded"),
                        );
                    }
                    if state_window_pending {
                        lazy_diagnostics.push(
                            TraceDiagnostic::warning(
                                "AppStorage writer dataflow is being built by another request. Retry after the reported pending job completes.",
                            )
                            .with_code("state_source_dataflow_already_building"),
                        );
                    }
                }
                Err(error) => {
                    partial = true;
                    lazy_diagnostics.push(
                        TraceDiagnostic::warning(&format!(
                            "AppStorage writer discovery failed: {error}"
                        ))
                        .with_code("state_source_discovery_failed"),
                    );
                }
            }
        }
        if let Some(summary) = lazy_summary.as_mut() {
            summary.duration_ms = lazy_start.elapsed().as_millis() as u64;
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
    fn facade_does_not_reexport_pipeline_mechanisms() {
        let facade = include_str!("lib.rs")
            .split("// ─── Engine")
            .next()
            .expect("facade declarations precede Engine implementation");
        for forbidden in [
            "ClosurePlanner",
            "DependencyClosure",
            "PrioritizedWorkset",
            "JobContext",
            "LanguageRegistry",
            "ParseWorkerPool",
            "ResolutionSession",
            "SummaryStore",
            "WorkerConfig",
            "build_dirty_set",
            "clean_stale_file_ids",
            "clean_stale_file_paths",
            "phase_",
            "run_index_pipeline",
            "source_file_id",
        ] {
            assert!(
                !facade.contains(forbidden),
                "facade must not re-export internal mechanism {forbidden}"
            );
        }
    }

    #[test]
    fn engine_constructs_in_memory() {
        let engine = Engine::open_in_memory().expect("should create in-memory engine");
        let _ = engine.store();
        let _ = engine.trace_engine();
    }

    #[test]
    fn engine_capabilities_match_enabled_languages() {
        let caps = Engine::all_capabilities();
        assert_eq!(caps.len(), Language::enabled_languages().len());
    }

    #[test]
    #[cfg(feature = "typescript")]
    fn engine_capability_for_typescript() {
        let cap = Engine::language_capability(Language::TypeScript);
        assert_eq!(cap.language, "typescript");
        assert!(
            cap.capability_level >= types::capability::CapabilityLevel::DataflowLocal,
            "TypeScript should be at least DataflowLocal"
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
        let source = decode_source(
            b"function add(a: number, b: number) {\n  return a + b;\n}\nadd(1, 2);\n",
        );
        let file_path = SourcePath::from_relative("test.ts");

        let facts = engine
            .extract_file_with_mode(
                &file_path,
                &source,
                Language::TypeScript,
                extraction::ExtractionMode::Full,
            )
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
    #[cfg(feature = "typescript")]
    fn engine_extract_preserves_raw_hash_for_non_utf8_source() {
        let engine = Engine::open_in_memory().expect("should create in-memory engine");
        let source = decode_source(b"function f() {}\n// \x80\n");
        assert_ne!(source.file_hash, source.text_hash());

        let facts = engine
            .extract_file_with_mode(
                &SourcePath::from_relative("legacy.ts"),
                &source,
                Language::TypeScript,
                extraction::ExtractionMode::Structural,
            )
            .expect("should extract decoded legacy source");

        assert_eq!(facts.file.content_hash, source.file_hash);
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
            &(),
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
            &(),
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
