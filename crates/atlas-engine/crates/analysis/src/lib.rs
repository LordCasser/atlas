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

pub mod branch_diff;
pub mod cfg_graph;
pub mod cross_function;
pub mod domain_rules;
pub mod lifecycle;
pub mod lifecycle_proof;
pub mod ownership_rules;
pub mod rule_learning;
pub mod summary;
pub mod trace;

pub use branch_diff::{BranchDiff, BranchDiffEngine, BranchPathSummary};
pub use lifecycle::{
    FieldLifecycleEngine, FieldLifecycleResult, FieldState, FieldTransition, OwnershipRules,
    SuspiciousKind, SuspiciousPoint,
};
pub use lifecycle_proof::{
    EvidenceLevel, LifecycleProof, LifecycleVerdict, PathProof, evaluate_proof,
};
pub use ownership_rules::CppOwnershipRules;
