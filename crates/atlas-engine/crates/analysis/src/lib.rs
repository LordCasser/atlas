//! Analysis layer: location-driven trace queries, call-graph exploration,
//! and lightweight function summaries.
//!
//! Architecture:
//! - `trace/` — variable tracking, caller-path, and trace-point resolution
//!   (production API exposed via CLI and MCP)
//! - `summary` — query-time FunctionSummary builder for intraprocedural
//!   reachability (parameter → return / call-arg / field)
//! - `cross_function` — inter-procedural bridging via persisted summaries
//!   (CrossFunctionBridge) with runtime fallback

pub mod cross_function;
pub mod summary;
pub mod trace;
pub mod cfg_graph;
pub mod lifecycle;
pub mod branch_diff;
pub mod domain_rules;
pub mod ownership_rules;
pub mod lifecycle_proof;
pub mod rule_learning;

pub use lifecycle::{
    FieldLifecycleEngine, FieldLifecycleResult, FieldState, FieldTransition, OwnershipRules,
    SuspiciousKind, SuspiciousPoint,
};
pub use branch_diff::{BranchDiff, BranchDiffEngine, BranchPathSummary};
pub use ownership_rules::CppOwnershipRules;
pub use lifecycle_proof::{EvidenceLevel, LifecycleProof, LifecycleVerdict, PathProof, evaluate_proof};
