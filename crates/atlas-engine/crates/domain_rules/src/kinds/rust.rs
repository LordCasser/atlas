//! Rust rule kind registry and learning strategy.
//!
//! Defines ownership rule kinds for Rust:
//! - `rust/alloc_fn`: functions that allocate/heap-allocate (Box::new, Arc::new, etc.)
//! - `rust/free_fn`: functions that explicitly deallocate/drop
//! - `rust/owned_pattern`: struct field patterns that indicate ownership
//! - `rust/cleanup_fn`: functions that perform cleanup (e.g., std::mem::forget)

use super::super::learning::{LearnedRuleCandidate, RuleLearningStrategy};
use super::super::registry::{LanguageRuleKinds, RuleKindSpec, RuleValidationResult};
use super::super::types::{DomainRule, PatternKind};

use db::Store;

/// Rust rule kind registry.
#[derive(Debug)]
pub struct RustRegistry;

impl LanguageRuleKinds for RustRegistry {
    fn language(&self) -> &'static str {
        "rust"
    }

    fn known_rule_kinds(&self) -> &'static [RuleKindSpec] {
        &[
            RuleKindSpec {
                name: "rust/alloc_fn",
                description: "Function that allocates memory or creates an owned resource (e.g., Box::new)",
                auto_learn_enabled: true,
                allowed_pattern_kinds: &[
                    PatternKind::Exact,
                    PatternKind::Prefix,
                    PatternKind::Glob,
                ],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "rust/free_fn",
                description: "Function that explicitly deallocates or drops a resource (e.g., drop, std::mem::drop)",
                auto_learn_enabled: true,
                allowed_pattern_kinds: &[
                    PatternKind::Exact,
                    PatternKind::Prefix,
                    PatternKind::Glob,
                ],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "rust/owned_pattern",
                description: "Struct field pattern indicating ownership (e.g., `data->ptr*`)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact, PatternKind::Prefix],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "rust/cleanup_fn",
                description: "Function that performs cleanup or escape (e.g., std::mem::forget)",
                auto_learn_enabled: true,
                allowed_pattern_kinds: &[PatternKind::Exact],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
        ]
    }

    fn builtin_rules(&self) -> Vec<DomainRule> {
        let now = String::new();
        let rules = [
            ("rust/alloc_fn", "Box::new", "exact"),
            ("rust/alloc_fn", "Vec::new", "exact"),
            ("rust/alloc_fn", "Arc::new", "exact"),
            ("rust/alloc_fn", "Rc::new", "exact"),
            ("rust/free_fn", "drop", "exact"),
            ("rust/free_fn", "std::mem::drop", "exact"),
            ("rust/cleanup_fn", "std::mem::forget", "exact"),
        ];
        rules
            .iter()
            .map(|(kind, pattern, pkind)| DomainRule {
                id: format!(
                    "rust_{}_{pattern}",
                    kind.replace("rust/", "").replace('/', "_")
                ),
                language: "rust".into(),
                rule_kind: kind.to_string(),
                pattern: pattern.to_string(),
                pattern_kind: pkind.to_string(),
                meta: None,
                meta_version: 1,
                source: "builtin".into(),
                status: "enabled".into(),
                confidence: 0.8,
                created_at: now.clone(),
                updated_at: now.clone(),
            })
            .collect()
    }

    fn validate_rule(&self, rule: &DomainRule) -> RuleValidationResult {
        crate::registry::default_validate_rule(self.known_rule_kinds(), "Rust", rule)
    }
}

/// Rust rule learning strategy (minimal stub).
///
/// Future: scan for `::new()` patterns, `fn drop()`, `fn forget()` conventions.
#[derive(Debug)]
pub struct RustLearningStrategy;

impl RuleLearningStrategy for RustLearningStrategy {
    fn language(&self) -> &'static str {
        "rust"
    }

    fn discover_candidates(&self, _store: &Store) -> anyhow::Result<Vec<LearnedRuleCandidate>> {
        // Stub: no auto-discovery for Rust yet.
        Ok(Vec::new())
    }

    fn explain_candidate(&self, candidate: &LearnedRuleCandidate) -> String {
        format!(
            "Rust function '{}' matched {} pattern",
            candidate.pattern, candidate.rule_kind
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_rules() {
        let reg = RustRegistry;
        let rules = reg.builtin_rules();
        assert!(!rules.is_empty());
        let alloc_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "rust/alloc_fn")
            .collect();
        let free_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "rust/free_fn")
            .collect();
        assert!(alloc_rules.iter().any(|r| r.pattern == "Box::new"));
        assert!(alloc_rules.iter().any(|r| r.pattern == "Arc::new"));
        assert!(free_rules.iter().any(|r| r.pattern == "drop"));
    }

    #[test]
    fn test_validate_valid_rule() {
        let reg = RustRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "rust".into(),
            rule_kind: "rust/alloc_fn".into(),
            pattern: "my_alloc".into(),
            pattern_kind: "exact".into(),
            meta: None,
            meta_version: 1,
            source: "user".into(),
            status: "enabled".into(),
            confidence: 1.0,
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert!(matches!(
            reg.validate_rule(&rule),
            RuleValidationResult::Valid
        ));
    }

    #[test]
    fn test_validate_unknown_kind() {
        let reg = RustRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "rust".into(),
            rule_kind: "unknown_kind".into(),
            pattern: "x".into(),
            pattern_kind: "exact".into(),
            meta: None,
            meta_version: 1,
            source: "user".into(),
            status: "enabled".into(),
            confidence: 1.0,
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert!(matches!(
            reg.validate_rule(&rule),
            RuleValidationResult::Rejected(_)
        ));
    }
}
