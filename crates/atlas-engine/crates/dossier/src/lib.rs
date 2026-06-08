//! Atlas dossier — Symbol Dossier for `atlas_explore`.
//!
//! The dossier crate provides the types, traits, and builder for the
//! redesigned `atlas_explore` tool. Instead of a shallow adjacency list
//! (`incoming` / `outgoing`), the dossier returns a comprehensive
//! Symbol Dossier with:
//!
//! - Source code excerpts
//! - Call graph evidence with call-site snippets
//! - Non-call relations grouped by category
//! - File-level context (imports, exports, peers)
//! - Recommended next queries
//!
//! ## Module structure
//!
//! - `types` — all dossier output types, request params, and internal enums
//! - `traits` — repository abstractions for data access
//! - `builder` — dossier assembly (placeholder for Phase 3)

pub mod builder;
pub mod file_facts_repo;
pub mod relation_repo;
pub mod source_repo;
pub mod symbol_repo;
pub mod traits;
pub mod types;

// Re-export core traits
pub use traits::FileFactsRepository;
pub use traits::RelationRepository;
pub use traits::SourceRepository;
pub use traits::SymbolRepository;

// Re-export implementations
pub use file_facts_repo::FileFactsRepo;
pub use relation_repo::RelationRepo;
pub use source_repo::SourceRepo;
pub use symbol_repo::SymbolRepo;
