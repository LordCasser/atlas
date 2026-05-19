//! CLI command implementations.

pub mod context;
pub mod doctor;
pub mod files;
pub mod index;
pub mod init;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod search;
pub mod status;
pub mod sync;
