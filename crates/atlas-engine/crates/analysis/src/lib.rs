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

pub mod alias_table;
pub mod branch_diff;
pub mod branch_diff_semantic;
pub mod cfg_graph;
pub mod cross_function;
pub mod effect_composer;
pub mod lifecycle;
pub mod lifecycle_proof;
pub mod ownership_rules;
pub mod resource_ops;
pub mod scope_exit;
pub mod summary;
pub mod trace;

pub use branch_diff::{BranchDiff, BranchDiffEngine, BranchPathSummary};
pub use branch_diff_semantic::{
    BranchAsymmetryKind, BranchDiffIssue, FieldEffectSummary, IssueSeverity,
    analyze_branch_semantic,
};
pub use effect_composer::{
    EffectComposition, FieldFreeRecord, FieldWriteRecord, TransferGraph, compose_effects,
};
pub use lifecycle::{
    FieldLifecycleEngine, FieldLifecycleResult, FieldState, FieldTransition, OwnershipRules,
    SuspiciousKind, SuspiciousPoint,
};
pub use lifecycle_proof::{
    EvidenceLevel, LifecycleProof, LifecycleVerdict, PathProof, evaluate_proof,
};
pub use ownership_rules::CppOwnershipRules;
// Re-export external domain-rules crate (previously domain_rules.rs)
pub use domain_rules;
pub use resource_ops::{CalleeMatcher, ResourceOpConfig, ResourceOpKind, ResourceOpPattern};

/// Rule learning — delegates to language-specific RuleLearningStrategy.
/// Inline module replaces `rule_learning.rs`.
pub mod rule_learning {
    pub use domain_rules::learning::{LearnedRuleCandidate, LearningEvidence, RuleLearningStrategy};
}
