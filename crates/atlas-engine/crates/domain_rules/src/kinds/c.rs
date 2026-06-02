//! C/C++ rule kind registry and learning strategy.
//!
//! Defines the four core C/C++ ownership rule kinds:
//! - `free_fn`: functions that deallocate memory
//! - `alloc_fn`: functions that allocate memory
//! - `owned_pattern`: struct field patterns that indicate ownership
//! - `cleanup_fn`: functions that perform cleanup

use super::super::learning::{LearnedRuleCandidate, LearningEvidence, RuleLearningStrategy};
use super::super::registry::{LanguageRuleKinds, RuleKindSpec, RuleValidationResult};
use super::super::types::{DomainRule, PatternKind, RuleSource, RuleStatus};

use db::Store;

/// C/C++ rule kind registry.
#[derive(Debug)]
pub struct CRegistry;

impl LanguageRuleKinds for CRegistry {
    fn language(&self) -> &'static str {
        "c"
    }

    fn known_rule_kinds(&self) -> &'static [RuleKindSpec] {
        &[
            RuleKindSpec {
                name: "free_fn",
                description: "Function that deallocates or releases memory/resources",
                auto_learn_enabled: true,
                allowed_pattern_kinds: &[
                    PatternKind::Exact,
                    PatternKind::Prefix,
                    PatternKind::Glob,
                ],
                default_status: status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "alloc_fn",
                description: "Function that allocates memory or creates a resource",
                auto_learn_enabled: true,
                allowed_pattern_kinds: &[
                    PatternKind::Exact,
                    PatternKind::Prefix,
                    PatternKind::Glob,
                ],
                default_status: status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "owned_pattern",
                description: "Struct field pattern indicating ownership (e.g., 'data->ptr*')",
                auto_learn_enabled: false,
                allowed_pattern_kinds: &[PatternKind::Exact, PatternKind::Prefix],
                default_status: status_for_source,
                meta_validator: None,
            },
            RuleKindSpec {
                name: "cleanup_fn",
                description: "Function that performs cleanup but not necessarily free",
                auto_learn_enabled: true,
                allowed_pattern_kinds: &[PatternKind::Exact],
                default_status: status_for_source,
                meta_validator: None,
            },
        ]
    }

    fn builtin_rules(&self) -> Vec<DomainRule> {
        let now = String::new();
        let rules = [
            ("free_fn", "free", "exact"),
            ("free_fn", "delete", "exact"),
            ("free_fn", "operator delete", "exact"),
            ("alloc_fn", "malloc", "exact"),
            ("alloc_fn", "calloc", "exact"),
            ("alloc_fn", "realloc", "exact"),
            ("alloc_fn", "strdup", "exact"),
            ("alloc_fn", "operator new", "exact"),
        ];
        rules
            .iter()
            .map(|(kind, pattern, pkind)| DomainRule {
                id: format!("c_{kind}_{pattern}"),
                language: "c".into(),
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
                    "Unknown rule_kind '{}' for C/C++. Known kinds: {:?}",
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

/// C/C++ rule learning strategy.
///
/// Scans function names across the project for patterns matching
/// free/release/destroy (free_fn) and alloc/create/make (alloc_fn)
/// conventions.
#[derive(Debug)]
pub struct CLearningStrategy;

impl RuleLearningStrategy for CLearningStrategy {
    fn language(&self) -> &'static str {
        "c"
    }

    fn discover_candidates(
        &self,
        store: &Store,
    ) -> anyhow::Result<Vec<LearnedRuleCandidate>> {
        let mut candidates: Vec<LearnedRuleCandidate> = Vec::new();

        let names = store.query_function_names()?;
        for name in &names {
            // Check free-like patterns
            if let Some(kind) = match_free_pattern(name) {
                candidates.push(LearnedRuleCandidate {
                    language: "c".into(),
                    rule_kind: kind,
                    pattern: name.clone(),
                    pattern_kind: PatternKind::Exact,
                    usage_count: 1,
                    confidence: 0.0,
                    evidence: vec![LearningEvidence {
                        file_id: String::new(),
                        symbol_id: Some(name.clone()),
                        line: 0,
                        evidence_kind: "name_pattern".into(),
                        confidence: 0.5,
                    }],
                });
            }
            // Check alloc-like patterns
            if let Some(kind) = match_alloc_pattern(name) {
                candidates.push(LearnedRuleCandidate {
                    language: "c".into(),
                    rule_kind: kind,
                    pattern: name.clone(),
                    pattern_kind: PatternKind::Exact,
                    usage_count: 1,
                    confidence: 0.0,
                    evidence: vec![LearningEvidence {
                        file_id: String::new(),
                        symbol_id: Some(name.clone()),
                        line: 0,
                        evidence_kind: "name_pattern".into(),
                        confidence: 0.5,
                    }],
                });
            }
        }

        // Deduplicate by name (sum usage counts)
        let mut merged: std::collections::HashMap<String, LearnedRuleCandidate> =
            std::collections::HashMap::new();
        for c in candidates {
            let key = format!("{}_{}", c.rule_kind, c.pattern);
            merged
                .entry(key)
                .and_modify(|existing| existing.usage_count += 1)
                .or_insert(c);
        }

        // Compute confidence
        let mut result: Vec<_> = merged.into_values().collect();
        for c in &mut result {
            c.confidence = (0.5 + (c.usage_count as f64 / 10.0).min(0.35)).min(0.85);
        }
        result.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(result)
    }

    fn explain_candidate(&self, candidate: &LearnedRuleCandidate) -> String {
        format!(
            "function '{}' matched {} pattern based on {} usage sites",
            candidate.pattern, candidate.rule_kind, candidate.usage_count
        )
    }
}

fn match_free_pattern(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    if lower.contains("free")
        || lower.contains("release")
        || lower.contains("destroy")
        || lower.contains("cleanup")
        || lower.contains("clear")
        || lower.contains("close")
        || lower.contains("delete")
        || lower.contains("dispose")
        || lower.contains("drop")
    {
        Some("free_fn".into())
    } else {
        None
    }
}

fn match_alloc_pattern(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    if lower.contains("alloc")
        || lower.contains("create")
        || lower.contains("new_")
        || lower.contains("make_")
        || lower.contains("build_")
        || lower.contains("init_")
        || lower.contains("open_")
        || lower.contains("clone_")
        || lower.contains("dup")
    {
        Some("alloc_fn".into())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_rules() {
        let reg = CRegistry;
        let rules = reg.builtin_rules();
        assert!(!rules.is_empty());
        let free_rules: Vec<_> = rules.iter().filter(|r| r.rule_kind == "free_fn").collect();
        let alloc_rules: Vec<_> = rules.iter().filter(|r| r.rule_kind == "alloc_fn").collect();
        assert!(free_rules.iter().any(|r| r.pattern == "free"));
        assert!(alloc_rules.iter().any(|r| r.pattern == "malloc"));
    }

    #[test]
    fn test_validate_valid_rule() {
        let reg = CRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "c".into(),
            rule_kind: "free_fn".into(),
            pattern: "my_free".into(),
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
        let reg = CRegistry;
        let rule = DomainRule {
            id: "test".into(),
            language: "c".into(),
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

    #[test]
    fn test_match_free_pattern() {
        assert!(match_free_pattern("my_free_buffer").is_some());
        assert!(match_free_pattern("release_resource").is_some());
        assert!(match_free_pattern("destroy").is_some());
        assert!(match_free_pattern("cleanup_state").is_some());
        assert!(match_free_pattern("calculate").is_none());
    }

    #[test]
    fn test_match_alloc_pattern() {
        assert!(match_alloc_pattern("allocate_buffer").is_some());
        assert!(match_alloc_pattern("create_context").is_some());
        assert!(match_alloc_pattern("make_request").is_some());
        assert!(match_alloc_pattern("build_tree").is_some());
        assert!(match_alloc_pattern("free").is_none());
    }

    #[test]
    fn test_match_patterns_case_insensitive() {
        assert!(match_free_pattern("CURL_FREE").is_some());
        assert!(match_alloc_pattern("MALLOC_WRAPPER").is_some());
    }
}
