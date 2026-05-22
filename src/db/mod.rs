//! SQLite persistence layer: schema, Store (CRUD + FTS5 search).
//!
//! `Store` wraps a `Mutex<Connection>` and provides the primary API for
//! reading and writing all Atlas types to SQLite.

mod schema;
mod store;
pub(crate) mod store_fts;
pub(crate) mod store_rows;
pub(crate) mod store_writers;
pub(crate) mod readers;

// Reader traits deferred — inherent StoreReader methods used directly.
// pub use readers::{...} will be added when crate boundary requires trait dispatch.

pub use schema::{CURRENT_SCHEMA_VERSION, SCHEMA_DDL};
pub use store::{Store, StoreStats};
