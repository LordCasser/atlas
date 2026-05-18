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

pub use engine::{QueryCapture, QueryResults, run_queries, run_queries_text};
pub use extract::extract_file;
pub use grammar::LanguageRegistry;
pub use languages::LanguageAdapter;
