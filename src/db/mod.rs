//! SQLite persistence layer: schema, Store (CRUD + FTS5 search).
//!
//! `Store` wraps a `Mutex<Connection>` and provides the primary API for
//! reading and writing all Atlas types to SQLite.

mod schema;
mod store;

pub use schema::{CURRENT_SCHEMA_VERSION, MIN_READABLE_VERSION, SCHEMA_DDL};
pub use store::{Store, StoreStats};
