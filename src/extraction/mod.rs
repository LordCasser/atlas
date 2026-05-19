//! Extraction layer: tree-sitter AST parsing via `LanguageAdapter` trait.
//!
//! Architecture:
//! - `LanguageAdapter` — per-language query-driven interface (definitions, references, imports, scopes)
//! - `LanguageRegistry` — loads and caches tree-sitter grammars for enabled languages
//! - `engine` — standalone query runner (QueryCapture-based, for testing/debugging)
//! - `extract` — main extraction pipeline (parses source, runs queries, normalizes into FileFacts)
//!
//! Extraction never writes final edges — that's the resolver's job.

mod engine;
mod extract;
mod grammar;
pub mod languages;
mod scope_tree;
mod symbol_registry;

pub use engine::{QueryCapture, QueryResults, run_queries, run_queries_text};
pub use extract::extract_file;
pub use grammar::LanguageRegistry;
pub use languages::{create_adapter, LanguageAdapter};
pub use scope_tree::build_scope_tree;
pub use symbol_registry::{all_edge_sources_known, all_reference_sources_known, SymbolRegistry};
