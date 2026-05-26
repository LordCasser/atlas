//! Persistence layer: SQLite — Store, schema, FTS5 search.

pub mod readers;
mod schema;
mod store;
pub(crate) mod store_fts;
pub mod store_rows;
pub(crate) mod store_writers;

pub use readers::{CallGraphReader, DataflowReader, FileReader, SymbolReader, TraceStore};
pub use schema::{CURRENT_SCHEMA_VERSION, MIGRATIONS, SCHEMA_DDL, SchemaStatus, check_and_migrate};
pub use store::{Store, StoreStats};

// Re-export summary types for the analysis layer
pub mod summary {
    //! Re-exports from `store::summary` for cross-crate access.
    pub use crate::store::summary::{
        CallArgSourceRow, ParamReachRow, ReturnSourceRow, SummaryBuildStats, SummaryStore,
    };
}
