//! Go rule kind registry and learning strategy.
//!
//! Defines ownership rule kinds for Go:
//! - `go/alloc_fn`: functions that create/open resources (os.Open, sql.Open, etc.)
//! - `go/free_fn`: functions that close/release resources (.Close(), close() on channels)
//! - `go/escape_fn`: patterns where resources escape the current scope (goroutines)
//! - `go/cleanup_fn`: functions that perform cleanup beyond simple close

use super::super::learning::{LearnedRuleCandidate, RuleLearningStrategy};
use super::super::registry::{LanguageRuleKinds, RuleKindSpec};
use super::super::types::{DomainRule, PatternKind};

use db::Store;

/// Go rule kind registry.
#[derive(Debug)]
pub struct GoRegistry;

impl LanguageRuleKinds for GoRegistry {
    fn language(&self) -> &'static str {
        "go"
    }

    fn display_name(&self) -> &'static str {
        "Go"
    }

    fn known_rule_kinds(&self) -> &'static [RuleKindSpec] {
        &[
            RuleKindSpec {
                name: "go/alloc_fn",
                description: "Function that creates or opens a resource (e.g., os.Open, sql.Open)",
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
                name: "go/free_fn",
                description: "Function that closes or releases a resource (e.g., .Close(), close())",
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
                name: "go/escape_fn",
                description: "Pattern where a resource escapes the local scope (e.g., goroutines)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact, PatternKind::Prefix],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "go/cleanup_fn",
                description: "Function that performs cleanup (defer-style or explicit)",
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
            ("go/alloc_fn", "os.Open", "exact"),
            ("go/alloc_fn", "os.Create", "exact"),
            ("go/alloc_fn", "sql.Open", "exact"),
            ("go/alloc_fn", "net.Dial", "exact"),
            ("go/free_fn", "Close()", "suffix"),
            ("go/free_fn", "close()", "suffix"),
            ("go/escape_fn", "go func", "prefix"),
        ];
        rules
            .iter()
            .map(|(kind, pattern, pkind)| DomainRule {
                id: format!("go_{}_{pattern}", kind.replace("go/", "").replace('/', "_")),
                language: "go".into(),
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

/// Go rule learning strategy (minimal stub).
///
/// Future: scan for `*Open()`, `*Close()`, `defer` patterns.
#[derive(Debug)]
pub struct GoLearningStrategy;

impl RuleLearningStrategy for GoLearningStrategy {
    fn language(&self) -> &'static str {
        "go"
    }

    fn discover_candidates(&self, _store: &Store) -> anyhow::Result<Vec<LearnedRuleCandidate>> {
        // Stub: no auto-discovery for Go yet.
        Ok(Vec::new())
    }

}

#[cfg(test)]
mod tests {
    use super::super::super::registry::RuleValidationResult;
    use super::*;

    #[test]
    fn test_builtin_rules() {
        let reg = GoRegistry;
        let rules = reg.builtin_rules();
        assert!(!rules.is_empty());
        let alloc_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "go/alloc_fn")
            .collect();
        let free_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "go/free_fn")
            .collect();
        assert!(alloc_rules.iter().any(|r| r.pattern == "os.Open"));
        assert!(alloc_rules.iter().any(|r| r.pattern == "sql.Open"));
        assert!(free_rules.iter().any(|r| r.pattern == "Close()"));
    }

    #[test]
    fn test_validate_valid_rule() {
        let reg = GoRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "go".into(),
            rule_kind: "go/alloc_fn".into(),
            pattern: "my.Open".into(),
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
        let reg = GoRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "go".into(),
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
