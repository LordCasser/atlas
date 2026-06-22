//! C/C++ ownership rules — consumer of the language-agnostic domain_rules crate.
//!
//! Loads rules from the generic rule engine and interprets them as C/C++
//! ownership semantics (free functions, allocation functions, owned field
//! patterns, cleanup functions).

use crate::builtins;
use db::Store;
use domain_rules::{RuleMatch, RuleSource};
use types::enums::Language;

/// C/C++ ownership rules — loaded from the generic rule engine and interpreted
/// as ownership semantics.
#[derive(Debug, Clone, Default)]
pub struct CppOwnershipRules {
    pub free_functions: Vec<(String, RuleSource)>,
    pub allocation_functions: Vec<(String, RuleSource)>,
    pub owned_field_patterns: Vec<String>,
    pub cleanup_functions: Vec<(String, RuleSource)>,
}

impl CppOwnershipRules {
    /// Load rules from the database for the given language.
    pub fn load(
        _engine: &domain_rules::GenericRuleEngine,
        store: &Store,
        language: Language,
    ) -> Self {
        Self::load_for(store, language.as_str())
    }

    /// Load rules for a specific language from the database.
    pub fn load_for(store: &Store, lang: &str) -> Self {
        let mut rules = Self::default();

        if let Ok(rows) = store.list_domain_rules(None, None) {
            for row in rows {
                if row.language != lang && row.language != "*" {
                    continue;
                }
                if row.status != "enabled" {
                    continue;
                }
                let source = match row.source.as_str() {
                    "builtin" => RuleSource::Builtin,
                    "learned" => RuleSource::Learned,
                    _ => RuleSource::User,
                };
                match row.rule_kind.as_str() {
                    "free_fn" => rules.free_functions.push((row.pattern, source)),
                    "alloc_fn" => rules.allocation_functions.push((row.pattern, source)),
                    "owned_pattern" => rules.owned_field_patterns.push(row.pattern),
                    "cleanup_fn" => rules.cleanup_functions.push((row.pattern, source)),
                    _ => {}
                }
            }
        }

        rules
    }

    /// Match a function name against free function rules + builtin defaults.
    pub fn match_free(&self, func_name: &str) -> Option<RuleMatch> {
        for (pattern, source) in &self.free_functions {
            if pattern == func_name {
                return Some(match source {
                    RuleSource::User | RuleSource::Learned => RuleMatch::Known {
                        rule_id: format!("c_free_fn_{pattern}"),
                        kind: "free_fn".into(),
                        confidence: 1.0,
                        meta: None,
                    },
                    RuleSource::Builtin => RuleMatch::Heuristic {
                        rule_id: format!("c_free_fn_{pattern}"),
                        kind: "free_fn".into(),
                        confidence: 0.8,
                        meta: None,
                    },
                });
            }
        }
        // Builtin defaults for common C/C++ functions
        if builtins::C_FREE_FUNCTIONS.contains(&func_name) {
            return Some(RuleMatch::Heuristic {
                rule_id: "builtin_free".into(),
                kind: "free_fn".into(),
                confidence: 0.9,
                meta: None,
            });
        }
        None
    }

    /// Match a function name against allocation function rules + builtin defaults.
    pub fn match_alloc(&self, func_name: &str) -> Option<RuleMatch> {
        for (pattern, source) in &self.allocation_functions {
            if pattern == func_name {
                return Some(match source {
                    RuleSource::User | RuleSource::Learned => RuleMatch::Known {
                        rule_id: format!("c_alloc_fn_{pattern}"),
                        kind: "alloc_fn".into(),
                        confidence: 1.0,
                        meta: None,
                    },
                    RuleSource::Builtin => RuleMatch::Heuristic {
                        rule_id: format!("c_alloc_fn_{pattern}"),
                        kind: "alloc_fn".into(),
                        confidence: 0.8,
                        meta: None,
                    },
                });
            }
        }
        if builtins::C_ALLOC_FUNCTIONS.contains(&func_name) {
            return Some(RuleMatch::Heuristic {
                rule_id: "builtin_alloc".into(),
                kind: "alloc_fn".into(),
                confidence: 0.9,
                meta: None,
            });
        }
        None
    }

    /// Check if a field path matches an owned field pattern.
    pub fn matches_owned_pattern(&self, field_path: &str) -> bool {
        for pattern in &self.owned_field_patterns {
            if field_path.starts_with(pattern.trim_end_matches('*')) {
                return true;
            }
        }
        false
    }

    /// Whether any rules have been loaded from the database
    /// (beyond the builtin defaults).
    pub fn has_any_rules(&self) -> bool {
        !self.free_functions.is_empty()
            || !self.allocation_functions.is_empty()
            || !self.owned_field_patterns.is_empty()
            || !self.cleanup_functions.is_empty()
    }

    /// Whether any loaded rules were annotated by the user
    /// (source = "user").
    pub fn has_user_rules(&self) -> bool {
        self.free_functions
            .iter()
            .any(|(_, s)| matches!(s, RuleSource::User))
            || self
                .allocation_functions
                .iter()
                .any(|(_, s)| matches!(s, RuleSource::User))
            || self
                .cleanup_functions
                .iter()
                .any(|(_, s)| matches!(s, RuleSource::User))
    }
}

// ── OwnershipContract impl ─────────────────────────────────────────────────

use types::effects::{
    ConsumptionContract, ConsumptionStyle, OwnershipContract, ResourceLocator, ReturnContract,
};

impl OwnershipContract for CppOwnershipRules {
    fn classify_return(&self, callee: &str) -> Option<ReturnContract> {
        // 1. 查询 DB 加载的 alloc_fn 规则
        for (pattern, _source) in &self.allocation_functions {
            if pattern == callee {
                return Some(ReturnContract::NewOwned);
            }
        }
        // 2. 内置默认
        if builtins::C_ALLOC_FUNCTIONS.contains(&callee) {
            return Some(ReturnContract::NewOwned);
        }
        if builtins::C_MAYBE_OWNED.contains(&callee) {
            return Some(ReturnContract::MaybeOwned);
        }
        None
    }

    fn classify_consumption(&self, callee: &str) -> Option<ConsumptionContract> {
        // 1. 查询 DB 加载的 free_fn 规则
        for (pattern, _source) in &self.free_functions {
            if pattern == callee {
                return Some(ConsumptionContract {
                    resource: ResourceLocator::Argument { index: 0 },
                    style: ConsumptionStyle::ExplicitCall,
                    confidence: 0.9,
                });
            }
        }
        // 2. 内置默认
        if builtins::C_FREE_FUNCTIONS.contains(&callee) {
            return Some(ConsumptionContract {
                resource: ResourceLocator::Argument { index: 0 },
                style: ConsumptionStyle::ExplicitCall,
                confidence: 0.9,
            });
        }
        // 3. C++ destructor (implicit scope exit)
        if callee.starts_with('~') {
            return Some(ConsumptionContract {
                resource: ResourceLocator::ImplicitScopeExit,
                style: ConsumptionStyle::Implicit,
                confidence: 0.7,
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpp_ownership_rules_defaults() {
        let rules = CppOwnershipRules::default();
        assert!(rules.match_free("free").is_some());
        assert!(rules.match_free("operator delete").is_some());
        assert!(rules.match_alloc("malloc").is_some());
        assert!(rules.match_alloc("calloc").is_some());
        assert!(rules.match_alloc("operator new").is_some());
        assert!(rules.match_free("nonexistent_func").is_none());
    }

    #[test]
    fn test_cpp_ownership_rules_owned_pattern_match() {
        let mut rules = CppOwnershipRules::default();
        rules.owned_field_patterns.push("data->state.ptr*".into());
        assert!(rules.matches_owned_pattern("data->state.ptr.cookie"));
        assert!(rules.matches_owned_pattern("data->state.ptr"));
        assert!(!rules.matches_owned_pattern("other->field"));
    }

    #[test]
    fn test_custom_rules_change_alloc_detection() {
        // Default rules don't know custom names
        let default = CppOwnershipRules::default();
        assert!(default.match_alloc("custom_pool_alloc").is_none());
        assert!(default.match_alloc("my_mempool_get").is_none());

        // Add custom alloc rules
        let mut custom = CppOwnershipRules::default();
        custom
            .allocation_functions
            .push(("custom_pool_alloc".into(), RuleSource::User));
        custom
            .allocation_functions
            .push(("my_mempool_get".into(), RuleSource::User));

        // Custom names now match
        assert!(custom.match_alloc("custom_pool_alloc").is_some());
        assert!(custom.match_alloc("my_mempool_get").is_some());
        // Builtin defaults should still work alongside custom ones
        assert!(custom.match_alloc("malloc").is_some());
    }

    #[test]
    fn test_custom_rules_change_free_detection() {
        let default = CppOwnershipRules::default();
        assert!(default.match_free("custom_pool_free").is_none());
        assert!(default.match_free("my_mempool_put").is_none());

        let mut custom = CppOwnershipRules::default();
        custom
            .free_functions
            .push(("custom_pool_free".into(), RuleSource::User));
        custom
            .free_functions
            .push(("my_mempool_put".into(), RuleSource::User));

        assert!(custom.match_free("custom_pool_free").is_some());
        assert!(custom.match_free("my_mempool_put").is_some());
        // Builtin defaults should still work
        assert!(custom.match_free("free").is_some());
    }

    #[test]
    fn test_custom_rules_change_owned_field_matching() {
        let default = CppOwnershipRules::default();
        // No default patterns match "secret->*" fields
        assert!(!default.matches_owned_pattern("secret->internal_buf"));

        let mut custom = CppOwnershipRules::default();
        custom.owned_field_patterns.push("secret->*".into());

        assert!(custom.matches_owned_pattern("secret->internal_buf"));
        assert!(custom.matches_owned_pattern("secret->ptr"));
    }

    #[test]
    fn test_match_returns_confidence() {
        let mut rules = CppOwnershipRules::default();

        // Builtin heuristic returns Heuristic variant
        let m = rules.match_alloc("malloc").unwrap();
        assert!(matches!(m, RuleMatch::Heuristic { .. }));

        // User-defined rule returns Known variant
        rules
            .allocation_functions
            .push(("myalloc".into(), RuleSource::User));
        let m = rules.match_alloc("myalloc").unwrap();
        assert!(matches!(m, RuleMatch::Known { .. }));

        // Non-matches return None
        assert!(rules.match_alloc("printf").is_none());
        assert!(rules.match_free("printf").is_none());
    }
}
