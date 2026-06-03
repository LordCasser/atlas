//! Java rule kind registry and learning strategy.
//!
//! Defines ownership rule kinds for Java:
//! - `java/alloc_fn`: resource allocation (constructor calls, open/connect factories)
//! - `java/free_fn`: resource release (.close(), .dispose(), .destroy())
//! - `java/try_resource`: try-with-resources managed resources (scope-level analysis)
//! - `java/cleanup_fn`: general cleanup functions

use super::super::learning::{LearnedRuleCandidate, RuleLearningStrategy};
use super::super::registry::{LanguageRuleKinds, RuleKindSpec, RuleValidationResult};
use super::super::types::{DomainRule, PatternKind, RuleSource, RuleStatus};

use db::Store;

/// Java rule kind registry.
#[derive(Debug)]
pub struct JavaRegistry;

impl LanguageRuleKinds for JavaRegistry {
    fn language(&self) -> &'static str {
        "java"
    }

    fn known_rule_kinds(&self) -> &'static [RuleKindSpec] {
        &[
            RuleKindSpec {
                name: "java/alloc_fn",
                description:
                    "Function that creates or opens a resource (e.g., Files.newInputStream, DriverManager.getConnection)",
                auto_learn_enabled: true,
                allowed_pattern_kinds: &[
                    PatternKind::Exact,
                    PatternKind::Prefix,
                    PatternKind::Suffix,
                    PatternKind::Glob,
                ],
                default_status: status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "java/free_fn",
                description:
                    "Function that closes or releases a resource (e.g., .close(), .dispose(), .destroy())",
                auto_learn_enabled: true,
                allowed_pattern_kinds: &[
                    PatternKind::Exact,
                    PatternKind::Suffix,
                    PatternKind::Glob,
                ],
                default_status: status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "java/try_resource",
                description: "try-with-resources managed resources (scope-level resource handling)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact, PatternKind::Prefix],
                default_status: status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "java/cleanup_fn",
                description: "General cleanup functions (e.g., finalize(), @Cleanup annotation)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact],
                default_status: status_for_source,
                meta_validator: None,
            },
        ]
    }

    fn builtin_rules(&self) -> Vec<DomainRule> {
        let now = String::new();
        let rules = [
            ("java/alloc_fn", "Files.newInputStream", "exact"),
            ("java/alloc_fn", "Files.newOutputStream", "exact"),
            ("java/alloc_fn", "DriverManager.getConnection", "exact"),
            ("java/alloc_fn", "openConnection", "suffix"),
            ("java/alloc_fn", "openStream", "suffix"),
            ("java/alloc_fn", "getConnection", "suffix"),
            ("java/free_fn", ".close", "suffix"),
            ("java/free_fn", ".dispose", "suffix"),
            ("java/free_fn", ".destroy", "suffix"),
        ];
        rules
            .iter()
            .map(|(kind, pattern, pkind)| DomainRule {
                id: format!(
                    "java_{}_{pattern}",
                    kind.replace("java/", "").replace('/', "_")
                ),
                language: "java".into(),
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
        // Check rule_kind is registered
        let known = self.known_rule_kinds();
        let spec = match known.iter().find(|s| s.name == rule.rule_kind) {
            Some(s) => s,
            None => {
                return RuleValidationResult::Rejected(format!(
                    "Unknown rule_kind '{}' for Java. Known kinds: {:?}",
                    rule.rule_kind,
                    known.iter().map(|s| s.name).collect::<Vec<_>>()
                ));
            }
        };

        // Check pattern_kind is allowed
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
}

fn status_for_source(source: RuleSource) -> RuleStatus {
    match source {
        RuleSource::Builtin => RuleStatus::Enabled,
        RuleSource::Learned => RuleStatus::Candidate,
        RuleSource::User => RuleStatus::Enabled,
    }
}

/// Java rule learning strategy (minimal stub).
///
/// Future: scan for try-with-resources, .close(), .dispose(), .destroy(), AutoCloseable implementations.
#[derive(Debug)]
pub struct JavaLearningStrategy;

impl RuleLearningStrategy for JavaLearningStrategy {
    fn language(&self) -> &'static str {
        "java"
    }

    fn discover_candidates(&self, _store: &Store) -> anyhow::Result<Vec<LearnedRuleCandidate>> {
        // Stub: no auto-discovery for Java yet.
        Ok(Vec::new())
    }

    fn explain_candidate(&self, candidate: &LearnedRuleCandidate) -> String {
        format!(
            "Java method '{}' matched {} pattern",
            candidate.pattern, candidate.rule_kind
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_rules() {
        let reg = JavaRegistry;
        let rules = reg.builtin_rules();
        assert!(!rules.is_empty());
        let alloc_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "java/alloc_fn")
            .collect();
        let free_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "java/free_fn")
            .collect();
        assert!(alloc_rules
            .iter()
            .any(|r| r.pattern == "Files.newInputStream"));
        assert!(free_rules.iter().any(|r| r.pattern == ".close"));
        assert!(free_rules.iter().any(|r| r.pattern == ".dispose"));
    }

    #[test]
    fn test_validate_valid_rule() {
        let reg = JavaRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "java".into(),
            rule_kind: "java/alloc_fn".into(),
            pattern: "myFactory".into(),
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
        let reg = JavaRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "java".into(),
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
