//! TypeScript / JavaScript rule kind registry and learning strategy.
//!
//! Defines ownership rule kinds for TypeScript/JavaScript:
//! - `ts/alloc_fn`: functions that create/open resources (open, createReadStream, etc.)
//! - `ts/free_fn`: functions that close/dispose/destroy resources (.close, .dispose, etc.)
//! - `ts/react_hook`: React hook boundaries (useEffect, useMemo, useCallback)
//! - `ts/cleanup_return`: return function from useEffect for cleanup (not yet implemented)

use super::super::learning::{LearnedRuleCandidate, RuleLearningStrategy};
use super::super::registry::{LanguageRuleKinds, RuleKindSpec, RuleValidationResult};
use super::super::types::{DomainRule, PatternKind, RuleSource, RuleStatus};

use db::Store;

/// TypeScript / JavaScript rule kind registry.
#[derive(Debug)]
pub struct TypeScriptRegistry;

impl LanguageRuleKinds for TypeScriptRegistry {
    fn language(&self) -> &'static str {
        "typescript"
    }

    fn known_rule_kinds(&self) -> &'static [RuleKindSpec] {
        &[
            RuleKindSpec {
                name: "ts/alloc_fn",
                description: "Function that creates or opens a resource (e.g., open, createReadStream)",
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
                name: "ts/free_fn",
                description: "Function that closes, disposes, or destroys a resource (e.g., .close(), .dispose())",
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
                name: "ts/react_hook",
                description: "React hook boundary (useEffect, useMemo, useCallback — resource lifecycle)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact, PatternKind::Prefix],
                default_status: status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "ts/cleanup_return",
                description: "Cleanup function returned from useEffect (not yet implemented)",
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
            ("ts/alloc_fn", "open", "exact"),
            ("ts/alloc_fn", "createReadStream", "exact"),
            ("ts/alloc_fn", "createWriteStream", "exact"),
            ("ts/alloc_fn", "createServer", "exact"),
            ("ts/alloc_fn", "createClient", "exact"),
            ("ts/alloc_fn", "setTimeout", "exact"),
            ("ts/alloc_fn", "setInterval", "exact"),
            ("ts/free_fn", ".dispose", "suffix"),
            ("ts/free_fn", ".close", "suffix"),
            ("ts/free_fn", ".destroy", "suffix"),
            ("ts/free_fn", ".release", "suffix"),
            ("ts/free_fn", "clearTimeout", "exact"),
            ("ts/free_fn", "clearInterval", "exact"),
            ("ts/react_hook", "useEffect", "exact"),
            ("ts/react_hook", "useMemo", "exact"),
            ("ts/react_hook", "useCallback", "exact"),
        ];
        rules
            .iter()
            .map(|(kind, pattern, pkind)| DomainRule {
                id: format!("ts_{}_{pattern}", kind.replace("ts/", "").replace('/', "_")),
                language: "typescript".into(),
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
                    "Unknown rule_kind '{}' for TypeScript. Known kinds: {:?}",
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

/// TypeScript rule learning strategy (minimal stub).
///
/// Future: scan for `create*()`, `.close()`, `useEffect` cleanup patterns.
#[derive(Debug)]
pub struct TypeScriptLearningStrategy;

impl RuleLearningStrategy for TypeScriptLearningStrategy {
    fn language(&self) -> &'static str {
        "typescript"
    }

    fn discover_candidates(&self, _store: &Store) -> anyhow::Result<Vec<LearnedRuleCandidate>> {
        // Stub: no auto-discovery for TypeScript yet.
        Ok(Vec::new())
    }

    fn explain_candidate(&self, candidate: &LearnedRuleCandidate) -> String {
        format!(
            "TypeScript function '{}' matched {} pattern",
            candidate.pattern, candidate.rule_kind
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_rules() {
        let reg = TypeScriptRegistry;
        let rules = reg.builtin_rules();
        assert!(!rules.is_empty());
        let alloc_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "ts/alloc_fn")
            .collect();
        let free_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "ts/free_fn")
            .collect();
        let hook_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "ts/react_hook")
            .collect();
        assert!(alloc_rules.iter().any(|r| r.pattern == "open"));
        assert!(alloc_rules.iter().any(|r| r.pattern == "createReadStream"));
        assert!(free_rules.iter().any(|r| r.pattern == ".close"));
        assert!(hook_rules.iter().any(|r| r.pattern == "useEffect"));
    }

    #[test]
    fn test_validate_valid_rule() {
        let reg = TypeScriptRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "typescript".into(),
            rule_kind: "ts/alloc_fn".into(),
            pattern: "myCreate".into(),
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
        let reg = TypeScriptRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "typescript".into(),
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
