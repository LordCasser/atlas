//! Rust rule kind registry and learning strategy.
//!
//! Defines ownership rule kinds for Rust:
//! - `rust/alloc_fn`: functions that allocate/heap-allocate (Box::new, Arc::new, etc.)
//! - `rust/free_fn`: functions that explicitly deallocate/drop
//! - `rust/owned_pattern`: struct field patterns that indicate ownership
//! - `rust/cleanup_fn`: functions that perform cleanup (e.g., std::mem::forget)

use super::super::learning::RuleLearningStrategy;
use super::super::registry::{LanguageRuleKinds, RuleKindSpec};
use super::super::types::{DomainRule, PatternKind};

/// Rust rule kind registry.
#[derive(Debug)]
pub struct RustRegistry;

impl LanguageRuleKinds for RustRegistry {
    fn language(&self) -> &'static str {
        "rust"
    }

    fn display_name(&self) -> &'static str {
        "Rust"
    }

    fn known_rule_kinds(&self) -> &'static [RuleKindSpec] {
        &[
            RuleKindSpec {
                name: "rust/alloc_fn",
                description: "Function that allocates memory or creates an owned resource (e.g., Box::new)",
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
                name: "rust/free_fn",
                description: "Function that explicitly deallocates or drops a resource (e.g., drop, std::mem::drop)",
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
                name: "rust/owned_pattern",
                description: "Struct field pattern indicating ownership (e.g., `data->ptr*`)",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact, PatternKind::Prefix],
                default_status: crate::registry::status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "rust/cleanup_fn",
                description: "Function that performs cleanup or escape (e.g., std::mem::forget)",
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
            ("rust/alloc_fn", "Box::new", "exact"),
            ("rust/alloc_fn", "Vec::new", "exact"),
            ("rust/alloc_fn", "Arc::new", "exact"),
            ("rust/alloc_fn", "Rc::new", "exact"),
            ("rust/free_fn", "drop", "exact"),
            ("rust/free_fn", "std::mem::drop", "exact"),
            ("rust/cleanup_fn", "std::mem::forget", "exact"),
        ];
        rules
            .iter()
            .map(|(kind, pattern, pkind)| DomainRule {
                id: format!(
                    "rust_{}_{pattern}",
                    kind.replace("rust/", "").replace('/', "_")
                ),
                language: "rust".into(),
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

/// Rust rule learning strategy (minimal stub).
///
/// Future: scan for `::new()` patterns, `fn drop()`, `fn forget()` conventions.
#[derive(Debug)]
pub struct RustLearningStrategy;

impl RuleLearningStrategy for RustLearningStrategy {
    fn language(&self) -> &'static str {
        "rust"
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_rules() {
        let reg = RustRegistry;
        let rules = reg.builtin_rules();
        assert!(!rules.is_empty());
        let alloc_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "rust/alloc_fn")
            .collect();
        let free_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.rule_kind == "rust/free_fn")
            .collect();
        assert!(alloc_rules.iter().any(|r| r.pattern == "Box::new"));
        assert!(alloc_rules.iter().any(|r| r.pattern == "Arc::new"));
        assert!(free_rules.iter().any(|r| r.pattern == "drop"));
    }
}
