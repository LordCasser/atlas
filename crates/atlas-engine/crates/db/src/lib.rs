//! Persistence layer: SQLite (primary) and DuckDB (exploratory bulk-write backend).

pub mod duck;
pub mod readers;
mod schema;
mod store;
pub(crate) mod store_fts;
pub mod store_rows;
pub(crate) mod store_writers;

pub use readers::{CallGraphReader, DataflowReader, FileReader, SymbolReader, TraceStore};
pub use schema::{CURRENT_SCHEMA_VERSION, MIGRATIONS, SCHEMA_DDL, SchemaStatus, check_and_migrate};
pub use store::{Store, StoreStats};
