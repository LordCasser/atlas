//! Kotlin rule kind registry and learning strategy.
//!
//! Defines ownership rule kinds for Kotlin:
//! - `kotlin/alloc_fn`: resource allocation (File constructors, factory functions)
//! - `kotlin/free_fn`: resource release (.close(), .dispose(), .use())
//! - `kotlin/coroutine`: coroutine-scoped resources (launch, async, withContext, coroutineScope)
//! - `kotlin/cleanup_fn`: general cleanup functions

use super::super::learning::{LearnedRuleCandidate, RuleLearningStrategy};
use super::super::registry::{LanguageRuleKinds, RuleKindSpec, RuleValidationResult};
use super::super::types::{DomainRule, PatternKind, RuleSource, RuleStatus};

use db::Store;

/// Kotlin rule kind registry.
#[derive(Debug)]
pub struct KotlinRegistry;

impl LanguageRuleKinds for KotlinRegistry {
    fn language(&self) -> &'static str {
        "kotlin"
    }

    fn known_rule_kinds(&self) -> &'static [RuleKindSpec] {
        &[
            RuleKindSpec {
                name: "kotlin/alloc_fn",
                description: "Function that creates or opens a resource (e.g., File(), bufferedReader(), bufferedWriter())",
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
                name: "kotlin/free_fn",
                description: "Function that closes or releases a resource (e.g., .close(), .dispose(), .use())",
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
                name: "kotlin/coroutine",
                description: "Coroutine-scoped resource patterns (launch, async, withContext, coroutineScope)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact, PatternKind::Suffix],
                default_status: status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "kotlin/cleanup_fn",
                description: "General cleanup functions",
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
            ("kotlin/alloc_fn", "File", "suffix"),
            ("kotlin/alloc_fn", "bufferedReader", "suffix"),
            ("kotlin/alloc_fn", "bufferedWriter", "suffix"),
            ("kotlin/alloc_fn", "openConnection", "suffix"),
            ("kotlin/free_fn", ".use", "exact"),
            ("kotlin/free_fn", ".close", "suffix"),
            ("kotlin/free_fn", ".dispose", "suffix"),
            ("kotlin/coroutine", "launch", "exact"),
            ("kotlin/coroutine", "async", "exact"),
            ("kotlin/coroutine", "withContext", "exact"),
            ("kotlin/coroutine", "coroutineScope", "exact"),
        ];
        rules
            .iter()
            .map(|(kind, pattern, pkind)| DomainRule {
                id: format!(
                    "kotlin_{}_{pattern}",
                    kind.replace("kotlin/", "").replace('/', "_")
                ),
                language: "kotlin".into(),
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
        let known = self.known_rule_kinds();
        let spec = match known.iter().find(|s| s.name == rule.rule_kind) {
            Some(s) => s,
            None => {
                return RuleValidationResult::Rejected(format!(
                    "Unknown rule_kind '{}' for Kotlin. Known kinds: {:?}",
                    rule.rule_kind,
                    known.iter().map(|s| s.name).collect::<Vec<_>>()
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
}

fn status_for_source(source: RuleSource) -> RuleStatus {
    match source {
        RuleSource::Builtin => RuleStatus::Enabled,
        RuleSource::Learned => RuleStatus::Candidate,
        RuleSource::User => RuleStatus::Enabled,
    }
}

/// Kotlin rule learning strategy (minimal stub).
///
/// Future: scan for .use() calls, coroutine scopes, Closeable implementations.
#[derive(Debug)]
pub struct KotlinLearningStrategy;

impl RuleLearningStrategy for KotlinLearningStrategy {
    fn language(&self) -> &'static str {
        "kotlin"
    }

    fn discover_candidates(&self, _store: &Store) -> anyhow::Result<Vec<LearnedRuleCandidate>> {
        Ok(Vec::new())
    }

    fn explain_candidate(&self, candidate: &LearnedRuleCandidate) -> String {
        format!(
            "Kotlin function '{}' matched {} pattern",
            candidate.pattern, candidate.rule_kind
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_rules() {
        let reg = KotlinRegistry;
        let rules = reg.builtin_rules();
        assert!(!rules.is_empty());
        let alloc_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "kotlin/alloc_fn")
            .collect();
        let free_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "kotlin/free_fn")
            .collect();
        assert!(alloc_rules.iter().any(|r| r.pattern == "File"));
        assert!(free_rules.iter().any(|r| r.pattern == ".close"));
        assert!(free_rules.iter().any(|r| r.pattern == ".use"));
    }

    #[test]
    fn test_validate_valid_rule() {
        let reg = KotlinRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "kotlin".into(),
            rule_kind: "kotlin/alloc_fn".into(),
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
        let reg = KotlinRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "kotlin".into(),
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
