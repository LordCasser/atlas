//! Language rule kind registry — defines what rule kinds each language supports.

use super::types::{DomainRule, PatternKind, RuleSource, RuleStatus};

pub type MetaValidator = fn(&serde_json::Value) -> Result<(), String>;

/// Specification for a single rule kind within a language.
#[derive(Debug, Clone)]
pub struct RuleKindSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub auto_learn_enabled: bool,
    pub allowed_pattern_kinds: &'static [PatternKind],
    pub default_status: fn(RuleSource) -> RuleStatus,
    pub meta_validator: Option<MetaValidator>,
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

    /// Human-readable display name for error messages.
    fn display_name(&self) -> &'static str;

    /// Validate a domain rule against this language's kind specifications.
    fn validate_rule(&self, rule: &DomainRule) -> RuleValidationResult {
        default_validate_rule(self.known_rule_kinds(), self.display_name(), rule)
    }
}

/// Standard status-for-source mapping shared by all language registries.
pub fn status_for_source(source: RuleSource) -> RuleStatus {
    match source {
        RuleSource::Builtin => RuleStatus::Enabled,
        RuleSource::Learned => RuleStatus::Candidate,
        RuleSource::User => RuleStatus::Enabled,
    }
}

/// Default rule validation shared by all language registries.
/// Each language passes its own display name for error messages.
pub fn default_validate_rule(
    known_kinds: &[RuleKindSpec],
    lang_display: &str,
    rule: &DomainRule,
) -> RuleValidationResult {
    let spec = match known_kinds.iter().find(|s| s.name == rule.rule_kind) {
        Some(s) => s,
        None => {
            return RuleValidationResult::Rejected(format!(
                "Unknown rule_kind '{}' for {}. Known kinds: {:?}",
                rule.rule_kind,
                lang_display,
                known_kinds.iter().map(|s| s.name).collect::<Vec<_>>()
            ));
        }
    };
    let pkind = match PatternKind::from_str(&rule.pattern_kind) {
        Some(pk) => pk,
        None => {
            return RuleValidationResult::Rejected(format!(
                "Unknown pattern_kind '{}'",
                rule.pattern_kind
            ));
        }
    };
    if !spec.allowed_pattern_kinds.contains(&pkind) {
        return RuleValidationResult::Warning(format!(
            "pattern_kind '{}' is not in the allowed set for '{}': {:?}",
            pkind.as_str(),
            rule.rule_kind,
            spec.allowed_pattern_kinds
        ));
    }
    RuleValidationResult::Valid
}
