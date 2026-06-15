//! Java rule kind registry and learning strategy.
//!
//! Defines ownership rule kinds for Java:
//! - `java/alloc_fn`: resource allocation (constructor calls, open/connect factories)
//! - `java/free_fn`: resource release (.close(), .dispose(), .destroy())
//! - `java/try_resource`: try-with-resources managed resources (scope-level analysis)
//! - `java/cleanup_fn`: general cleanup functions

use super::super::learning::RuleLearningStrategy;
use super::super::registry::{LanguageRuleKinds, RuleKindSpec};
use super::super::types::{DomainRule, PatternKind};

/// Java rule kind registry.
#[derive(Debug)]
pub struct JavaRegistry;

impl LanguageRuleKinds for JavaRegistry {
    fn language(&self) -> &'static str {
        "java"
    }

    fn display_name(&self) -> &'static str {
        "Java"
    }

    fn known_rule_kinds(&self) -> &'static [RuleKindSpec] {
        &[
            RuleKindSpec {
                name: "java/alloc_fn",
                description: "Function that creates or opens a resource (e.g., Files.newInputStream, DriverManager.getConnection)",
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
                name: "java/free_fn",
                description: "Function that closes or releases a resource (e.g., .close(), .dispose(), .destroy())",
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
                name: "java/try_resource",
                description: "try-with-resources managed resources (scope-level resource handling)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact, PatternKind::Prefix],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "java/cleanup_fn",
                description: "General cleanup functions (e.g., finalize(), @Cleanup annotation)",
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
        assert!(
            alloc_rules
                .iter()
                .any(|r| r.pattern == "Files.newInputStream")
        );
        assert!(free_rules.iter().any(|r| r.pattern == ".close"));
        assert!(free_rules.iter().any(|r| r.pattern == ".dispose"));
    }
}
