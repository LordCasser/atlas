//! PHP rule kind registry and learning strategy.
//!
//! Defines ownership rule kinds for PHP:
//! - `php/alloc_fn`: resource allocation (fopen, mysqli_connect, curl_init)
//! - `php/free_fn`: resource release (fclose, mysqli_close, curl_close)
//! - `php/procedural_resource`: procedural resource management patterns
//! - `php/cleanup_fn`: general cleanup functions

use super::super::learning::{LearnedRuleCandidate, RuleLearningStrategy};
use super::super::registry::{LanguageRuleKinds, RuleKindSpec, RuleValidationResult};
use super::super::types::{DomainRule, PatternKind};

use db::Store;

/// PHP rule kind registry.
#[derive(Debug)]
pub struct PhpRegistry;

impl LanguageRuleKinds for PhpRegistry {
    fn language(&self) -> &'static str {
        "php"
    }

    fn known_rule_kinds(&self) -> &'static [RuleKindSpec] {
        &[
            RuleKindSpec {
                name: "php/alloc_fn",
                description: "Function that creates or opens a resource (e.g., fopen, mysqli_connect, curl_init)",
                auto_learn_enabled: true,
                allowed_pattern_kinds: &[
                    PatternKind::Exact,
                    PatternKind::Prefix,
                    PatternKind::Suffix,
                    PatternKind::Glob,
                ],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "php/free_fn",
                description: "Function that closes or releases a resource (e.g., fclose, mysqli_close, curl_close)",
                auto_learn_enabled: true,
                allowed_pattern_kinds: &[
                    PatternKind::Exact,
                    PatternKind::Suffix,
                    PatternKind::Glob,
                ],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "php/procedural_resource",
                description: "Procedural resource management patterns (fopen/fclose pairs, connection lifecycle)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact, PatternKind::Suffix],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "php/cleanup_fn",
                description: "General cleanup functions (e.g., __destruct, register_shutdown_function)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
        ]
    }

    fn builtin_rules(&self) -> Vec<DomainRule> {
        let now = String::new();
        let rules = [
            ("php/alloc_fn", "fopen", "exact"),
            ("php/alloc_fn", "mysqli_connect", "exact"),
            ("php/alloc_fn", "curl_init", "exact"),
            ("php/free_fn", "fclose", "exact"),
            ("php/free_fn", "mysqli_close", "exact"),
            ("php/free_fn", "curl_close", "exact"),
            ("php/procedural_resource", "fopen", "exact"),
            ("php/procedural_resource", "fclose", "exact"),
        ];
        rules
            .iter()
            .map(|(kind, pattern, pkind)| DomainRule {
                id: format!(
                    "php_{}_{pattern}",
                    kind.replace("php/", "").replace('/', "_")
                ),
                language: "php".into(),
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
        crate::registry::default_validate_rule(self.known_rule_kinds(), "PHP", rule)
    }
}

/// PHP rule learning strategy (minimal stub).
///
/// Future: scan for fopen/fclose pairs, mysqli_connect/close, curl_init/close patterns.
#[derive(Debug)]
pub struct PhpLearningStrategy;

impl RuleLearningStrategy for PhpLearningStrategy {
    fn language(&self) -> &'static str {
        "php"
    }

    fn discover_candidates(&self, _store: &Store) -> anyhow::Result<Vec<LearnedRuleCandidate>> {
        Ok(Vec::new())
    }

    fn explain_candidate(&self, candidate: &LearnedRuleCandidate) -> String {
        format!(
            "PHP function '{}' matched {} pattern",
            candidate.pattern, candidate.rule_kind
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_rules() {
        let reg = PhpRegistry;
        let rules = reg.builtin_rules();
        assert!(!rules.is_empty());
        let alloc_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "php/alloc_fn")
            .collect();
        let free_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "php/free_fn")
            .collect();
        assert!(alloc_rules.iter().any(|r| r.pattern == "fopen"));
        assert!(free_rules.iter().any(|r| r.pattern == "fclose"));
    }

    #[test]
    fn test_validate_valid_rule() {
        let reg = PhpRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "php".into(),
            rule_kind: "php/alloc_fn".into(),
            pattern: "myOpen".into(),
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
        let reg = PhpRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "php".into(),
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
