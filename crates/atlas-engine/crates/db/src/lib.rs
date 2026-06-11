//! Persistence layer: SQLite — Store, schema, FTS5 search.

pub mod bulk_schema;
pub mod readers;
mod schema;
mod store;
pub(crate) mod store_fts;
pub mod store_rows;
pub(crate) mod store_writers;

pub use readers::{CallGraphReader, DataflowReader, FileReader, SymbolReader, TraceStore};
pub use schema::{CURRENT_SCHEMA_VERSION, SCHEMA_DDL};
pub use store::domain_rules::DomainRuleRow;
pub use store::extraction_jobs::{ClaimResult, ExtractionJob};
pub use store::{FullRebuildGuard, Store, StoreStats, WalCheckpointStats};
pub use store::{IndexMode, KEY_GRAPH_GENERATION, KEY_RESOLUTION_CONFIG_HASH, KEY_RESOLUTION_GENERATION};

// Re-export summary types for the analysis layer
pub mod summary {
    //! Re-exports from `store::summary` for cross-crate access.
    pub use crate::store::summary::{
        CallArgSourceRow, ParamReachRow, ReturnSourceRow, SummaryBuildStats, SummaryStore,
    };
}
