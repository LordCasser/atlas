//! Focus-driven incremental analysis types.
//!
//! This module defines the core types for Atlas's focus-driven analysis:
//! - [`FocusSeed`] — what the user is looking at
//! - [`FocusWindow`] — seed + strategies + budget
//! - [`FocusClosure`] — the built closure with files/symbols/gaps
//! - [`FocusJobState`] — lifecycle of a focus extraction job
//!
//! Sub-modules:
//! - [`visibility_filter`] — language-specific visibility rules for closure symbols
//! - [`edge_policy`] — edge conflict resolution when building focus graphs
//! - [`scheduler`] — priority-queue scheduling for background focus jobs
//! - [`writer_coordinator`] — serialized DB write access

pub mod engine;
pub mod types;
pub mod atlas;
pub mod visibility_filter;
pub mod edge_policy;
pub mod scheduler;
pub mod writer_coordinator;

#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod types_tests;
#[cfg(test)]
mod visibility_filter_tests;
#[cfg(test)]
mod edge_policy_tests;
#[cfg(test)]
mod scheduler_tests;
#[cfg(test)]
mod writer_coordinator_tests;
#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod integration_tests_extended;
