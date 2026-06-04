//! Ruby rule kind registry and learning strategy.
//!
//! Defines ownership rule kinds for Ruby:
//! - `ruby/alloc_fn`: resource allocation (File.open, File.new, TCPSocket.new, Net::HTTP.start)
//! - `ruby/free_fn`: resource release (.close, .dispose)
//! - `ruby/block_resource`: block-based resource patterns (File.open { |f| ... })
//! - `ruby/cleanup_fn`: general cleanup functions

use super::super::learning::{LearnedRuleCandidate, RuleLearningStrategy};
use super::super::registry::{LanguageRuleKinds, RuleKindSpec, RuleValidationResult};
use super::super::types::{DomainRule, PatternKind, RuleSource, RuleStatus};

use db::Store;

/// Ruby rule kind registry.
#[derive(Debug)]
pub struct RubyRegistry;

impl LanguageRuleKinds for RubyRegistry {
    fn language(&self) -> &'static str {
        "ruby"
    }

    fn known_rule_kinds(&self) -> &'static [RuleKindSpec] {
        &[
            RuleKindSpec {
                name: "ruby/alloc_fn",
                description: "Function that creates or opens a resource (e.g., File.open, File.new, TCPSocket.new, Net::HTTP.start)",
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
                name: "ruby/free_fn",
                description: "Function that closes or releases a resource (e.g., .close, .dispose)",
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
                name: "ruby/block_resource",
                description: "Block-based resource management patterns (File.open with do...end block)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact, PatternKind::Suffix],
                default_status: status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "ruby/cleanup_fn",
                description: "General cleanup functions (e.g., ensure blocks, at_exit handlers)",
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
            ("ruby/alloc_fn", "File.open", "exact"),
            ("ruby/alloc_fn", "File.new", "exact"),
            ("ruby/alloc_fn", "TCPSocket.new", "exact"),
            ("ruby/alloc_fn", "Net::HTTP.start", "exact"),
            ("ruby/alloc_fn", ".open", "suffix"),
            ("ruby/alloc_fn", ".new", "suffix"),
            ("ruby/free_fn", ".close", "suffix"),
            ("ruby/free_fn", ".dispose", "suffix"),
            ("ruby/block_resource", "File.open", "exact"),
            ("ruby/block_resource", "IO.open", "exact"),
        ];
        rules
            .iter()
            .map(|(kind, pattern, pkind)| DomainRule {
                id: format!(
                    "ruby_{}_{pattern}",
                    kind.replace("ruby/", "").replace('/', "_")
                ),
                language: "ruby".into(),
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
                    "Unknown rule_kind '{}' for Ruby. Known kinds: {:?}",
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

/// Ruby rule learning strategy (minimal stub).
///
/// Future: scan for File.open blocks, IO patterns, .close calls.
#[derive(Debug)]
pub struct RubyLearningStrategy;

impl RuleLearningStrategy for RubyLearningStrategy {
    fn language(&self) -> &'static str {
        "ruby"
    }

    fn discover_candidates(&self, _store: &Store) -> anyhow::Result<Vec<LearnedRuleCandidate>> {
        Ok(Vec::new())
    }

    fn explain_candidate(&self, candidate: &LearnedRuleCandidate) -> String {
        format!(
            "Ruby method '{}' matched {} pattern",
            candidate.pattern, candidate.rule_kind
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_rules() {
        let reg = RubyRegistry;
        let rules = reg.builtin_rules();
        assert!(!rules.is_empty());
        let alloc_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "ruby/alloc_fn")
            .collect();
        let free_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "ruby/free_fn")
            .collect();
        assert!(alloc_rules.iter().any(|r| r.pattern == "File.open"));
        assert!(free_rules.iter().any(|r| r.pattern == ".close"));
    }

    #[test]
    fn test_validate_valid_rule() {
        let reg = RubyRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "ruby".into(),
            rule_kind: "ruby/alloc_fn".into(),
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
        let reg = RubyRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "ruby".into(),
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
