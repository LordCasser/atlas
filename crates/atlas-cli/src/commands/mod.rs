//! CLI command implementations.

pub mod doctor;
pub mod files;
pub mod index;
#[cfg(feature = "mcp")]
pub mod mcp;
pub(crate) mod progress;
pub mod status;
pub mod sync;
