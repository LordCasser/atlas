//! Generic rule engine — language-agnostic rule matching with plugin registry.

use std::collections::HashMap;

use super::pattern;
use super::registry::LanguageRuleKinds;
use super::types::*;
use db::Store;

/// A language-agnostic rule matching engine.
///
/// Registered [`LanguageRuleKinds`] plugins provide language-specific rule kind
/// definitions. The engine queries the database for rules and matches targets
/// against enabled rules.
pub struct GenericRuleEngine {
    registry: HashMap<String, Box<dyn LanguageRuleKinds>>,
}

impl GenericRuleEngine {
    /// Create a new empty engine.
    pub fn new() -> Self {
        Self {
            registry: HashMap::new(),
        }
    }

    /// Register a language plugin.
    pub fn register(&mut self, plugin: Box<dyn LanguageRuleKinds>) {
        let lang = plugin.language().to_string();
        self.registry.insert(lang, plugin);
    }

    /// Match a target against rules of a specific language + rule_kind.
    ///
    /// Only returns status=Enabled rules. Query order:
    /// 1. Exact language match
    /// 2. language="*" fallback
    ///
    /// Known rules (user/learned with high confidence) return [`RuleMatch::Known`];
    /// builtin or low-confidence rules return [`RuleMatch::Heuristic`].
    pub fn match_pattern(
        &self,
        store: &Store,
        language: &str,
        rule_kind: &str,
        target: &str,
    ) -> Vec<RuleMatch> {
        let mut results = Vec::new();

        // Load rules for exact language
        let mut process_rules = |lang: &str| {
            if let Ok(rows) = store.get_domain_rules_by_kind(rule_kind, Some(lang)) {
                for row in rows {
                    if row.status != "enabled" {
                        continue;
                    }
                    let rule: DomainRule = row.into();
                    if !pattern::match_pattern(&rule, target) {
                        continue;
                    }
                    let meta = rule
                        .meta
                        .as_ref()
                        .and_then(|m| serde_json::from_str(m).ok());
                    let source = RuleSource::from_str(&rule.source).unwrap_or(RuleSource::User);
                    let rm = match source {
                        RuleSource::Builtin if rule.confidence < 0.9 => RuleMatch::Heuristic {
                            rule_id: rule.id.clone(),
                            kind: rule.rule_kind.clone(),
                            confidence: rule.confidence,
                            meta,
                        },
                        RuleSource::Builtin => RuleMatch::Heuristic {
                            rule_id: rule.id.clone(),
                            kind: rule.rule_kind.clone(),
                            confidence: rule.confidence,
                            meta,
                        },
                        _ => RuleMatch::Known {
                            rule_id: rule.id.clone(),
                            kind: rule.rule_kind.clone(),
                            confidence: rule.confidence,
                            meta,
                        },
                    };
                    results.push(rm);
                }
            }
        };

        // 1. Exact language match
        process_rules(language);

        // 2. Wildcard language fallback (only if different from exact)
        if language != "*" {
            process_rules("*");
        }

        results
    }
}

impl Default for GenericRuleEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        store
    }

    #[test]
    fn test_engine_match_pattern_enabled_rule() {
        let store = test_store();
        store
            .upsert_domain_rule(
                "c", "free_fn", "my_free", "exact", "user", "enabled", 1.0, None,
            )
            .unwrap();

        let engine = GenericRuleEngine::new();
        let matches = engine.match_pattern(&store, "c", "free_fn", "my_free");
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn test_engine_ignores_disabled_rules() {
        let store = test_store();
        store
            .upsert_domain_rule(
                "c", "free_fn", "my_free", "exact", "user", "disabled", 1.0, None,
            )
            .unwrap();

        let engine = GenericRuleEngine::new();
        let matches = engine.match_pattern(&store, "c", "free_fn", "my_free");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_engine_wildcard_fallback() {
        let store = test_store();
        store
            .upsert_domain_rule(
                "*", "free_fn", "any_free", "exact", "user", "enabled", 1.0, None,
            )
            .unwrap();

        let engine = GenericRuleEngine::new();
        let matches = engine.match_pattern(&store, "c", "free_fn", "any_free");
        assert_eq!(matches.len(), 1);
    }

    // ── §6.4: General-purpose verification ──────────────────────────
    #[test]
    fn test_engine_unknown_language_returns_empty() {
        let store = test_store();
        store
            .upsert_domain_rule(
                "c", "free_fn", "my_free", "exact", "user", "enabled", 1.0, None,
            )
            .unwrap();

        let engine = GenericRuleEngine::new();
        // Query for "rust" — no rules exist for this language
        let matches = engine.match_pattern(&store, "rust", "free_fn", "my_free");
        assert!(
            matches.is_empty(),
            "Unknown language should return empty vec"
        );
    }

    // ── §6.2: Status filtering ──────────────────────────────────────
    #[test]
    fn test_engine_ignores_candidate_status() {
        let store = test_store();
        store
            .upsert_domain_rule(
                "c",
                "free_fn",
                "candidate_fn",
                "exact",
                "learned",
                "candidate",
                0.8,
                None,
            )
            .unwrap();

        let engine = GenericRuleEngine::new();
        let matches = engine.match_pattern(&store, "c", "free_fn", "candidate_fn");
        assert!(
            matches.is_empty(),
            "Candidate status rules should not be returned"
        );
    }

    #[test]
    fn test_engine_ignores_rejected_status() {
        let store = test_store();
        store
            .upsert_domain_rule(
                "c",
                "free_fn",
                "rejected_fn",
                "exact",
                "learned",
                "rejected",
                0.5,
                None,
            )
            .unwrap();

        let engine = GenericRuleEngine::new();
        let matches = engine.match_pattern(&store, "c", "free_fn", "rejected_fn");
        assert!(
            matches.is_empty(),
            "Rejected status rules should not be returned"
        );
    }

    #[test]
    fn test_engine_rule_kind_with_prefix_pattern() {
        let store = test_store();
        store
            .upsert_domain_rule(
                "c",
                "free_fn",
                "safefree_",
                "prefix",
                "user",
                "enabled",
                1.0,
                None,
            )
            .unwrap();

        let engine = GenericRuleEngine::new();
        let matches = engine.match_pattern(&store, "c", "free_fn", "safefree_buffer");
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn test_engine_exact_match_rejects_partial() {
        let store = test_store();
        store
            .upsert_domain_rule(
                "c", "alloc_fn", "malloc", "exact", "builtin", "enabled", 1.0, None,
            )
            .unwrap();

        let engine = GenericRuleEngine::new();
        // "my_malloc" should NOT match exact pattern "malloc"
        let matches = engine.match_pattern(&store, "c", "alloc_fn", "my_malloc");
        assert!(
            matches.is_empty(),
            "Exact match should reject partial string matches"
        );
    }
}
