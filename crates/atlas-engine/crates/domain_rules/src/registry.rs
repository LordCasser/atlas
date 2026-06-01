//! Language rule kind registry — defines what rule kinds each language supports.

use super::types::{DomainRule, PatternKind, RuleSource, RuleStatus};

/// Specification for a single rule kind within a language.
#[derive(Debug, Clone)]
pub struct RuleKindSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub auto_learn_enabled: bool,
    pub allowed_pattern_kinds: &'static [PatternKind],
    pub default_status: fn(RuleSource) -> RuleStatus,
    pub meta_validator: Option<fn(&serde_json::Value) -> Result<(), String>>,
}

/// Result of validating a domain rule against a language's kind specification.
#[derive(Debug, Clone)]
pub enum RuleValidationResult {
    Valid,
    Warning(String),
    Rejected(String),
}

/// Trait for language-specific rule kind registries.
///
/// Each language can define its own set of rule kinds (e.g., C has `free_fn`, `alloc_fn`;
/// Rust might have `drop_impl`, `unsafe_block`; Python might have `context_manager`, etc.).
pub trait LanguageRuleKinds: std::fmt::Debug + Send + Sync {
    /// The language identifier this registry handles.
    fn language(&self) -> &'static str;

    /// All rule kinds known to this language.
    fn known_rule_kinds(&self) -> &'static [RuleKindSpec];

    /// Builtin rules that can be seeded into the database.
    fn builtin_rules(&self) -> Vec<DomainRule>;

    /// Validate a domain rule against this language's kind specifications.
    fn validate_rule(&self, rule: &DomainRule) -> RuleValidationResult;
}
