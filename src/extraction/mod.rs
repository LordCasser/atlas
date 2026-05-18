//! Extraction layer: tree-sitter AST parsing via `LanguageAdapter` trait.
//!
//! Architecture:
//! - `LanguageAdapter` — per-language query-driven interface (definitions, references, imports, scopes)
//! - `LanguageRegistry` — loads and caches tree-sitter grammars for enabled languages
//!
//! Extraction never writes final edges — that's the resolver's job.

mod grammar;
pub mod languages;

pub use grammar::LanguageRegistry;
pub use languages::LanguageAdapter;
