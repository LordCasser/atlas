//! C# rule kind registry and learning strategy.
//!
//! Defines ownership rule kinds for C#:
//! - `csharp/alloc_fn`: resource allocation (constructors, factory methods)
//! - `csharp/free_fn`: resource release (.Dispose(), .Close())
//! - `csharp/idisposable`: IDisposable patterns (using statement, Dispose method)
//! - `csharp/cleanup_fn`: general cleanup functions

use super::super::learning::{LearnedRuleCandidate, RuleLearningStrategy};
use super::super::registry::{LanguageRuleKinds, RuleKindSpec};
use super::super::types::{DomainRule, PatternKind};

use db::Store;

/// C# rule kind registry.
#[derive(Debug)]
pub struct CSharpRegistry;

impl LanguageRuleKinds for CSharpRegistry {
    fn language(&self) -> &'static str {
        "csharp"
    }

    fn display_name(&self) -> &'static str {
        "C#"
    }

    fn known_rule_kinds(&self) -> &'static [RuleKindSpec] {
        &[
            RuleKindSpec {
                name: "csharp/alloc_fn",
                description: "Function that creates or opens a resource (e.g., File.Open, new FileStream, SqlConnection)",
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
                name: "csharp/free_fn",
                description: "Function that closes or releases a resource (e.g., .Dispose(), .Close())",
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
                name: "csharp/idisposable",
                description: "IDisposable pattern resources (using statement, Dispose method, IDisposable interface)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact, PatternKind::Suffix],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "csharp/cleanup_fn",
                description: "General cleanup functions (e.g., finalizers, ~ClassName destructors)",
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
            ("csharp/alloc_fn", "File.Open", "exact"),
            ("csharp/alloc_fn", "new FileStream", "exact"),
            ("csharp/alloc_fn", "SqlConnection", "suffix"),
            ("csharp/alloc_fn", "HttpClient", "suffix"),
            ("csharp/alloc_fn", "OpenConnection", "suffix"),
            ("csharp/alloc_fn", "OpenStream", "suffix"),
            ("csharp/free_fn", ".Dispose", "suffix"),
            ("csharp/free_fn", ".Close", "suffix"),
            ("csharp/idisposable", "IDisposable", "suffix"),
        ];
        rules
            .iter()
            .map(|(kind, pattern, pkind)| DomainRule {
                id: format!(
                    "csharp_{}_{pattern}",
                    kind.replace("csharp/", "").replace('/', "_")
                ),
                language: "csharp".into(),
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

/// C# rule learning strategy (minimal stub).
///
/// Future: scan for IDisposable implementations, using statements, .Dispose() calls.
#[derive(Debug)]
pub struct CSharpLearningStrategy;

impl RuleLearningStrategy for CSharpLearningStrategy {
    fn language(&self) -> &'static str {
        "csharp"
    }

    fn discover_candidates(&self, _store: &Store) -> anyhow::Result<Vec<LearnedRuleCandidate>> {
        Ok(Vec::new())
    }

    fn explain_candidate(&self, candidate: &LearnedRuleCandidate) -> String {
        format!(
            "C# method '{}' matched {} pattern",
            candidate.pattern, candidate.rule_kind
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::registry::RuleValidationResult;
    use super::*;

    #[test]
    fn test_builtin_rules() {
        let reg = CSharpRegistry;
        let rules = reg.builtin_rules();
        assert!(!rules.is_empty());
        let alloc_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "csharp/alloc_fn")
            .collect();
        let free_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "csharp/free_fn")
            .collect();
        assert!(alloc_rules.iter().any(|r| r.pattern == "File.Open"));
        assert!(free_rules.iter().any(|r| r.pattern == ".Dispose"));
        assert!(free_rules.iter().any(|r| r.pattern == ".Close"));
    }

    #[test]
    fn test_validate_valid_rule() {
        let reg = CSharpRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "csharp".into(),
            rule_kind: "csharp/alloc_fn".into(),
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
        let reg = CSharpRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "csharp".into(),
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
