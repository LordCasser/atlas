//! Python rule kind registry and learning strategy.
//!
//! Defines ownership rule kinds for Python:
//! - `python/alloc_fn`: functions that create/open resources (open, socket, connect)
//! - `python/free_fn`: functions that close/release resources (.close, .dispose, .release)
//! - `python/context_manager`: context manager entry/exit boundaries (with-statement)
//! - `python/decorator_boundary`: decorator wrapping effects (@contextmanager, etc.)

use super::super::learning::{LearnedRuleCandidate, RuleLearningStrategy};
use super::super::registry::{LanguageRuleKinds, RuleKindSpec, RuleValidationResult};
use super::super::types::{DomainRule, PatternKind};

use db::Store;

/// Python rule kind registry.
#[derive(Debug)]
pub struct PythonRegistry;

impl LanguageRuleKinds for PythonRegistry {
    fn language(&self) -> &'static str {
        "python"
    }

    fn known_rule_kinds(&self) -> &'static [RuleKindSpec] {
        &[
            RuleKindSpec {
                name: "python/alloc_fn",
                description: "Function that creates or opens a resource (e.g., open, sqlite3.connect)",
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
                name: "python/free_fn",
                description: "Function that closes or releases a resource (e.g., .close(), .dispose())",
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
                name: "python/context_manager",
                description: "Context manager entry/exit boundaries (with-statement scope)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact, PatternKind::Prefix],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "python/decorator_boundary",
                description: "Decorator wrapping effects (e.g., @contextmanager, @atexit.register)",
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
            ("python/alloc_fn", "open", "exact"),
            ("python/alloc_fn", "sqlite3.connect", "exact"),
            ("python/alloc_fn", "socket.socket", "exact"),
            ("python/free_fn", ".close", "suffix"),
            ("python/free_fn", ".dispose", "suffix"),
            ("python/free_fn", ".release", "suffix"),
            ("python/free_fn", "os.close", "exact"),
        ];
        rules
            .iter()
            .map(|(kind, pattern, pkind)| DomainRule {
                id: format!(
                    "python_{}_{pattern}",
                    kind.replace("python/", "").replace('/', "_")
                ),
                language: "python".into(),
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
        crate::registry::default_validate_rule(self.known_rule_kinds(), "Python", rule)
    }
}

/// Python rule learning strategy (minimal stub).
///
/// Future: scan for `open()`, `.close()`, `@contextmanager`, `with` statement patterns.
#[derive(Debug)]
pub struct PythonLearningStrategy;

impl RuleLearningStrategy for PythonLearningStrategy {
    fn language(&self) -> &'static str {
        "python"
    }

    fn discover_candidates(&self, _store: &Store) -> anyhow::Result<Vec<LearnedRuleCandidate>> {
        // Stub: no auto-discovery for Python yet.
        Ok(Vec::new())
    }

    fn explain_candidate(&self, candidate: &LearnedRuleCandidate) -> String {
        format!(
            "Python function '{}' matched {} pattern",
            candidate.pattern, candidate.rule_kind
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_rules() {
        let reg = PythonRegistry;
        let rules = reg.builtin_rules();
        assert!(!rules.is_empty());
        let alloc_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "python/alloc_fn")
            .collect();
        let free_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "python/free_fn")
            .collect();
        assert!(alloc_rules.iter().any(|r| r.pattern == "open"));
        assert!(alloc_rules.iter().any(|r| r.pattern == "sqlite3.connect"));
        assert!(free_rules.iter().any(|r| r.pattern == ".close"));
        assert!(free_rules.iter().any(|r| r.pattern == "os.close"));
    }

    #[test]
    fn test_validate_valid_rule() {
        let reg = PythonRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "python".into(),
            rule_kind: "python/alloc_fn".into(),
            pattern: "my_open".into(),
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
        let reg = PythonRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "python".into(),
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
