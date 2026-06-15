//! TypeScript / JavaScript rule kind registry and learning strategy.
//!
//! Defines ownership rule kinds for TypeScript/JavaScript:
//! - `ts/alloc_fn`: functions that create/open resources (open, createReadStream, etc.)
//! - `ts/free_fn`: functions that close/dispose/destroy resources (.close, .dispose, etc.)
//! - `ts/react_hook`: React hook boundaries (useEffect, useMemo, useCallback)
//! - `ts/cleanup_return`: return function from useEffect for cleanup (not yet implemented)

use super::super::learning::RuleLearningStrategy;
use super::super::registry::{LanguageRuleKinds, RuleKindSpec};
use super::super::types::{DomainRule, PatternKind};
use super::rules_from_static;

/// TypeScript / JavaScript rule kind registry.
#[derive(Debug)]
pub struct TypeScriptRegistry;

impl LanguageRuleKinds for TypeScriptRegistry {
    fn language(&self) -> &'static str {
        "typescript"
    }

    fn display_name(&self) -> &'static str {
        "TypeScript"
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
                default_status: crate::registry::status_for_source,
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
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "ts/react_hook",
                description: "React hook boundary (useEffect, useMemo, useCallback — resource lifecycle)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact, PatternKind::Prefix],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "ts/cleanup_return",
                description: "Cleanup function returned from useEffect (not yet implemented)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
        ]
    }

    fn builtin_rules(&self) -> Vec<DomainRule> {
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
        rules_from_static("typescript", "ts", Some("ts/"), &rules)
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
}
