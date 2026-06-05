//! Extraction layer: tree-sitter AST parsing with slot-based language frontends.
//!
//! Architecture:
//! - `LanguageFrontend` — slot-based per-language interface
//! - `FrontendParts` — named slot bundle for constructing a `LanguageFrontend`
//! - `LanguageRegistry` — loads and caches tree-sitter grammars for enabled languages
//! - `extract` — main extraction pipeline (parses source, runs queries, normalizes into FileFacts)
//! - `semantic_binder` — extraction-time source ownership and scope binding (wraps SymbolRegistry)
//! - `worker` — managed extraction with timeout, panic isolation, and error reporting (ParseWorkerPool)
//! - `callsite_spec` — per-language callsite extraction interface
//!
//! Extraction never writes final edges — that's the resolver's job.

pub mod callsite_spec;
pub mod cancel;
mod cfg_builder;
mod dataflow_builder;
pub mod error;
mod extract;
pub(crate) mod extraction_ctx;
pub mod frontend;
mod grammar;
pub(crate) mod languages;
mod lexical_binder;
pub mod mode;
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
pub use cancel::CancelCheck;
pub use cfg_builder::{CfgBuilder, CfgResult};
pub use dataflow_builder::{DataFlowBuilder, DataFlowResult};
pub use error::{ExtractionFailure, ExtractionFailureKind};
pub use extract::{extract_file, extract_file_with_mode, extract_file_with_mode_cancellable};
pub use frontend::{
    FrontendParts, ImportExtractorSpec, LanguageFrontend, LexicalBindingSpec, ParserSpec,
    ReferenceExtractorSpec, ScopeExtractorSpec, SymbolExtractorSpec,
};
pub use grammar::LanguageRegistry;
pub use languages::{available_languages, create_frontend};
pub use lexical_binder::{LexicalBinder, LexicalBindingResult};
pub use mode::{ExtractionMode, parse_analysis_mode};
pub use scope_tree::build_scope_tree;
pub use semantic_binder::SemanticBinder;
pub use symbol_registry::{SymbolRegistry, all_edge_sources_known, all_reference_sources_known};
pub use worker::{ParseWorkerPool, WorkerConfig};
