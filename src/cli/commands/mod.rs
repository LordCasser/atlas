//! CLI command implementations.

pub mod doctor;
pub mod index;
pub mod init;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod search;
pub mod status;
pub mod sync;
