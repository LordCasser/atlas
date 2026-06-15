//! Language-specific rule kind registries.
//!
//! Each language module defines a `LanguageRuleKinds` implementation
//! with its own set of rule kinds, builtin rules, and validation logic.

use super::types::DomainRule;

pub mod c;
pub mod csharp;
pub mod go;
pub mod java;
pub mod kotlin;
pub mod php;
pub mod python;
pub mod ruby;
pub mod rust;
pub mod typescript;

pub(crate) fn rules_from_static(
    language: &str,
    id_prefix: &str,
    rule_kind_prefix: Option<&str>,
    rules: &[(&str, &str, &str)],
) -> Vec<DomainRule> {
    let now = String::new();
    rules
        .iter()
        .map(|(kind, pattern, pkind)| {
            let id_kind = rule_kind_prefix
                .map(|prefix| kind.replace(prefix, ""))
                .unwrap_or_else(|| kind.to_string())
                .replace('/', "_");

            DomainRule {
                id: format!("{id_prefix}_{id_kind}_{pattern}"),
                language: language.into(),
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
            }
        })
        .collect()
}
