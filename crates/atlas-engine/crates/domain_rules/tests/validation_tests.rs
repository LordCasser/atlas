//! Consolidated validation tests for all language registries.
//!
//! Each registry is tested with its own known-valid rule_kind
//! and with an unknown rule_kind to confirm rejection.

use domain_rules::registry::{LanguageRuleKinds, RuleValidationResult};
use domain_rules::DomainRule;

/// Helper: build a DomainRule with the given language and rule_kind.
fn make_rule(language: &str, rule_kind: &str) -> DomainRule {
    DomainRule {
        id: "test".into(),
        language: language.into(),
        rule_kind: rule_kind.into(),
        pattern: "test_func".into(),
        pattern_kind: "exact".into(),
        meta: None,
        meta_version: 1,
        source: "user".into(),
        status: "enabled".into(),
        confidence: 1.0,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

/// Assert that a registry validates a valid kind.
fn assert_valid(reg: &dyn LanguageRuleKinds, lang: &str, valid_kind: &str) {
    let rule = make_rule(lang, valid_kind);
    assert!(
        matches!(reg.validate_rule(&rule), RuleValidationResult::Valid),
        "Expected Valid for language={lang} kind={valid_kind}"
    );
}

/// Assert that a registry rejects an unknown kind.
fn assert_rejects_unknown(reg: &dyn LanguageRuleKinds, lang: &str) {
    let rule = make_rule(lang, "unknown_kind");
    assert!(
        matches!(reg.validate_rule(&rule), RuleValidationResult::Rejected(_)),
        "Expected Rejected for language={lang}"
    );
}

#[test]
fn test_validate_valid_rules() {
    assert_valid(&domain_rules::kinds::c::CRegistry, "c", "free_fn");
    assert_valid(&domain_rules::kinds::csharp::CSharpRegistry, "csharp", "csharp/alloc_fn");
    assert_valid(&domain_rules::kinds::go::GoRegistry, "go", "go/alloc_fn");
    assert_valid(&domain_rules::kinds::java::JavaRegistry, "java", "java/alloc_fn");
    assert_valid(&domain_rules::kinds::kotlin::KotlinRegistry, "kotlin", "kotlin/alloc_fn");
    assert_valid(&domain_rules::kinds::php::PhpRegistry, "php", "php/alloc_fn");
    assert_valid(&domain_rules::kinds::python::PythonRegistry, "python", "python/alloc_fn");
    assert_valid(&domain_rules::kinds::ruby::RubyRegistry, "ruby", "ruby/alloc_fn");
    assert_valid(&domain_rules::kinds::rust::RustRegistry, "rust", "rust/alloc_fn");
    assert_valid(
        &domain_rules::kinds::typescript::TypeScriptRegistry,
        "typescript",
        "ts/alloc_fn",
    );
}

#[test]
fn test_validate_unknown_kinds() {
    assert_rejects_unknown(&domain_rules::kinds::c::CRegistry, "c");
    assert_rejects_unknown(&domain_rules::kinds::csharp::CSharpRegistry, "csharp");
    assert_rejects_unknown(&domain_rules::kinds::go::GoRegistry, "go");
    assert_rejects_unknown(&domain_rules::kinds::java::JavaRegistry, "java");
    assert_rejects_unknown(&domain_rules::kinds::kotlin::KotlinRegistry, "kotlin");
    assert_rejects_unknown(&domain_rules::kinds::php::PhpRegistry, "php");
    assert_rejects_unknown(&domain_rules::kinds::python::PythonRegistry, "python");
    assert_rejects_unknown(&domain_rules::kinds::ruby::RubyRegistry, "ruby");
    assert_rejects_unknown(&domain_rules::kinds::rust::RustRegistry, "rust");
    assert_rejects_unknown(&domain_rules::kinds::typescript::TypeScriptRegistry, "typescript");
}
