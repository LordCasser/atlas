//! Persistence layer: SQLite — Store, schema, FTS5 search.

pub mod readers;
mod schema;
mod store;
pub(crate) mod store_fts;
pub mod store_rows;
pub(crate) mod store_writers;

pub use readers::{CallGraphReader, DataflowReader, FileReader, SymbolReader, TraceStore};
pub use schema::{CURRENT_SCHEMA_VERSION, SCHEMA_DDL};
pub use store::lazy_jobs::ClaimResult;
pub use store::{Store, StoreStats};

// Re-export summary types for the analysis layer
pub mod summary {
    //! Re-exports from `store::summary` for cross-crate access.
    pub use crate::store::summary::{
        CallArgSourceRow, ParamReachRow, ReturnSourceRow, SummaryBuildStats, SummaryStore,
    };
}
