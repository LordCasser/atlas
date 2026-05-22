//! Extraction layer: tree-sitter AST parsing via `LanguageAdapter` trait.
//!
//! Architecture:
//! - `LanguageAdapter` — per-language query-driven interface (definitions, references, imports, scopes)
//! - `LanguageRegistry` — loads and caches tree-sitter grammars for enabled languages
//! - `engine` — standalone query runner (QueryCapture-based, for testing/debugging)
//! - `extract` — main extraction pipeline (parses source, runs queries, normalizes into FileFacts)
//! - `semantic_binder` — extraction-time source ownership and scope binding (wraps SymbolRegistry)
//! - `worker` — managed extraction with timeout, panic isolation, and error reporting (ParseWorkerPool)
//! - `callsite_spec` — per-language callsite extraction interface (replaces hardcoded AST walk in extract.rs)
//!
//! Extraction never writes final edges — that's the resolver's job.

pub mod callsite_spec;
mod cfg_builder;
mod dataflow_builder;
mod engine;
mod extract;
pub mod frontend;
mod grammar;
pub mod languages;
mod lexical_binder;
mod query_helpers;
mod scope_tree;
mod semantic_binder;
mod symbol_registry;
mod worker;

pub use callsite_spec::{
    CallKind, CallsiteExtractorSpec, CallsiteParts, GenericCallsiteExtractor, c_callsite_extractor,
    cangjie_callsite_extractor, java_callsite_extractor, python_callsite_extractor,
    ts_callsite_extractor,
};
pub use cfg_builder::{CfgBuilder, CfgResult};
pub use dataflow_builder::{DataFlowBuilder, DataFlowResult};
pub use engine::{QueryCapture, QueryResults, run_queries, run_queries_text};
pub use extract::extract_file;
pub use frontend::{
    DataflowSpec, ImportExtractorSpec, LanguageFrontend, LexicalBindingSpec, ParserSpec,
    ReferenceExtractorSpec, ScopeExtractorSpec, SymbolExtractorSpec, UnsupportedSpec,
};
pub use grammar::LanguageRegistry;
pub use languages::{LanguageAdapter, create_adapter, create_frontend};
pub use lexical_binder::{LexicalBinder, LexicalBindingResult};
pub use scope_tree::build_scope_tree;
pub use semantic_binder::SemanticBinder;
pub use symbol_registry::{SymbolRegistry, all_edge_sources_known, all_reference_sources_known};
pub use worker::{ParseWorkerPool, WorkerConfig};
