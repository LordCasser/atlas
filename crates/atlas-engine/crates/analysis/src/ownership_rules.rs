//! C/C++ ownership rules — consumer of the language-agnostic domain_rules crate.
//!
//! Loads rules from the generic rule engine and interprets them as C/C++
//! ownership semantics (free functions, allocation functions, owned field
//! patterns, cleanup functions).

use db::Store;
use domain_rules::{RuleMatch, RuleSource};

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
    /// Load rules from the database for language "c".
    pub fn load(_engine: &domain_rules::GenericRuleEngine, store: &Store) -> Self {
        Self::load_for(store, "c")
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
        if func_name == "free" || func_name == "operator delete" || func_name == "std::free" {
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
        if matches!(
            func_name,
            "malloc" | "calloc" | "realloc" | "strdup" | "operator new"
        ) {
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

// Backward compatibility alias.
#[deprecated(note = "use CppOwnershipRules instead")]
pub type LoadedDomainRules = CppOwnershipRules;

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
}
