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
//! experimental frontends (Bash, Cangjie).

use std::path::Path;
use std::sync::Arc;

// ─── Re-exports ────────────────────────────────────────────────────────────

/// All core IR types (SymbolDef, ReferenceUse, FileFacts, etc.).
pub use types::*;
/// Database store and schema version.
pub use db::{CURRENT_SCHEMA_VERSION, Store, MIGRATIONS};
/// Workspace abstractions.
pub use workspace::{ProjectRoot, SourcePath, Workspace};
/// Extraction layer: language frontends, parser pool, grammar registry.
pub use extraction::{
    LanguageFrontend, LanguageRegistry, ParseWorkerPool, WorkerConfig, create_frontend,
    extract_file,
};
/// Resolution layer: reference resolver, path aliases, config hashing.
pub use resolution::{
    PathAliasResolver, ReferenceResolver, ResolutionStats, commit_config_hashes,
    detect_config_change,
};
/// Graph layer: graph builder, query engine, snapshots.
pub use graph::{
    GraphBuilder, GraphBuilderStats, GraphEngine, GraphSnapshot, NodeIx, TraversalConfig,
    TraversalDirection,
};
/// Analysis layer: trace engine and query responses.
pub use analysis::trace;
pub use analysis::trace::{TraceEngine, TraceQueryResponse};
/// Search layer: FTS5 + fuzzy search engine.
pub use search::{SearchEngine, SearchOptions};
/// Context layer: AI context builder (callers, callees, peers).
pub use context::ContextBuilder;
/// Sync layer: incremental sync engine, file lock, file discovery.
pub use filesync::{SyncEngine, SyncStats, FileLock, discovery};

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
    trace: analysis::trace::TraceEngine,
}

impl Engine {
    // ── Constructors ───────────────────────────────────────────────────

    /// Open an existing database file.
    ///
    /// The database must have been created by `atlas init` or a prior run.
    /// The schema is NOT initialized here — that is the CLI's responsibility.
    pub fn open(db_path: &Path) -> anyhow::Result<Self> {
        let store = Store::open_db(db_path)?;
        let store = Arc::new(store);
        let trace = analysis::trace::TraceEngine::new(store.clone());
        Ok(Self { store, trace })
    }

    /// Open a database file with a project root for snippet extraction.
    ///
    /// When a project root is provided, trace results can include source
    /// code snippets from the file system.
    pub fn open_with_root(db_path: &Path, project_root: &Path) -> anyhow::Result<Self> {
        let store = Store::open_db(db_path)?;
        let store = Arc::new(store);
        let trace = analysis::trace::TraceEngine::new_with_root(store.clone(), project_root.to_path_buf());
        Ok(Self { store, trace })
    }

    /// Open an in-memory database (for testing).
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let store = Store::open_in_memory()?;
        store.init_schema()?;
        let store = Arc::new(store);
        let trace = analysis::trace::TraceEngine::new(store.clone());
        Ok(Self { store, trace })
    }

    /// Access the underlying database store.
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Access the underlying trace engine.
    pub fn trace_engine(&self) -> &analysis::trace::TraceEngine {
        &self.trace
    }

    // ── Extraction ─────────────────────────────────────────────────────

    /// Extract facts from a single source file.
    ///
    /// Returns [`FileFacts`] containing symbols, references, imports, scopes,
    /// and (if supported) dataflow facts.  Does NOT write to the database.
    pub fn extract_file(&self, path: &Path, source: &str, language: Language) -> anyhow::Result<FileFacts> {
        let frontend = extraction::create_frontend(language)
            .ok_or_else(|| anyhow::anyhow!("Language frontend not available for {:?}", language))?;
        let file_id = FileId::generate(path.to_string_lossy().as_ref());
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let facts = extraction::extract_file(&frontend, file_id, path, source, &content_hash)?;
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
    pub fn trace_variable(
        &self,
        file_id: &FileId,
        line: u32,
        column: u32,
        max_depth: usize,
    ) -> analysis::trace::TraceQueryResponse<TracePath> {
        self.trace.trace_variable(file_id, line, column, max_depth)
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
}
