//! SQLite persistence layer: schema, Store (CRUD + FTS5 search).
//!
//! `Store` wraps a `Mutex<Connection>` and provides the primary API for
//! reading and writing all Atlas types to SQLite.

pub mod readers;
mod schema;
mod store;
pub(crate) mod store_fts;
pub mod store_rows;
pub(crate) mod store_writers;

pub use readers::{CallGraphReader, DataflowReader, FileReader, SymbolReader, TraceStore};
pub use schema::{CURRENT_SCHEMA_VERSION, MIGRATIONS, SCHEMA_DDL, SchemaStatus, check_and_migrate};
pub use store::{Store, StoreStats};
