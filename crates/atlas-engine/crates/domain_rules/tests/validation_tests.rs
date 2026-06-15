//! Consolidated validation tests for all language registries.
//!
//! Each registry is tested with its own known-valid rule_kind
//! and with an unknown rule_kind to confirm rejection.

use domain_rules::DomainRule;
use domain_rules::registry::{LanguageRuleKinds, RuleValidationResult};

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

fn assert_builtin_template(reg: &dyn LanguageRuleKinds, language: &str, expected_ids: &[&str]) {
    let rules = reg.builtin_rules();
    assert!(!rules.is_empty(), "Expected builtin rules for {language}");

    for rule in &rules {
        assert_eq!(rule.language, language);
        assert!(
            reg.known_rule_kinds()
                .iter()
                .any(|kind| kind.name == rule.rule_kind)
        );
        assert!(rule.meta.is_none());
        assert_eq!(rule.meta_version, 1);
        assert_eq!(rule.source, "builtin");
        assert_eq!(rule.status, "enabled");
        assert!((rule.confidence - 0.8).abs() < f64::EPSILON);
        assert_eq!(rule.created_at, "");
        assert_eq!(rule.updated_at, "");
    }

    for expected_id in expected_ids {
        assert!(
            rules.iter().any(|rule| rule.id == *expected_id),
            "Expected builtin id {expected_id} for {language}"
        );
    }
}

#[test]
fn test_validate_valid_rules() {
    assert_valid(&domain_rules::kinds::c::CRegistry, "c", "free_fn");
    assert_valid(
        &domain_rules::kinds::csharp::CSharpRegistry,
        "csharp",
        "csharp/alloc_fn",
    );
    assert_valid(&domain_rules::kinds::go::GoRegistry, "go", "go/alloc_fn");
    assert_valid(
        &domain_rules::kinds::java::JavaRegistry,
        "java",
        "java/alloc_fn",
    );
    assert_valid(
        &domain_rules::kinds::kotlin::KotlinRegistry,
        "kotlin",
        "kotlin/alloc_fn",
    );
    assert_valid(
        &domain_rules::kinds::php::PhpRegistry,
        "php",
        "php/alloc_fn",
    );
    assert_valid(
        &domain_rules::kinds::python::PythonRegistry,
        "python",
        "python/alloc_fn",
    );
    assert_valid(
        &domain_rules::kinds::ruby::RubyRegistry,
        "ruby",
        "ruby/alloc_fn",
    );
    assert_valid(
        &domain_rules::kinds::rust::RustRegistry,
        "rust",
        "rust/alloc_fn",
    );
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
    assert_rejects_unknown(
        &domain_rules::kinds::typescript::TypeScriptRegistry,
        "typescript",
    );
}

#[test]
fn test_builtin_rule_template_fields() {
    assert_builtin_template(
        &domain_rules::kinds::c::CRegistry,
        "c",
        &["c_free_fn_free", "c_alloc_fn_malloc"],
    );
    assert_builtin_template(
        &domain_rules::kinds::csharp::CSharpRegistry,
        "csharp",
        &["csharp_alloc_fn_File.Open", "csharp_free_fn_.Dispose"],
    );
    assert_builtin_template(
        &domain_rules::kinds::go::GoRegistry,
        "go",
        &["go_alloc_fn_os.Open", "go_free_fn_Close()"],
    );
    assert_builtin_template(
        &domain_rules::kinds::java::JavaRegistry,
        "java",
        &["java_alloc_fn_Files.newInputStream", "java_free_fn_.close"],
    );
    assert_builtin_template(
        &domain_rules::kinds::kotlin::KotlinRegistry,
        "kotlin",
        &["kotlin_alloc_fn_File", "kotlin_free_fn_.use"],
    );
    assert_builtin_template(
        &domain_rules::kinds::php::PhpRegistry,
        "php",
        &["php_alloc_fn_fopen", "php_procedural_resource_fclose"],
    );
    assert_builtin_template(
        &domain_rules::kinds::python::PythonRegistry,
        "python",
        &["python_alloc_fn_open", "python_free_fn_os.close"],
    );
    assert_builtin_template(
        &domain_rules::kinds::ruby::RubyRegistry,
        "ruby",
        &["ruby_alloc_fn_File.open", "ruby_block_resource_IO.open"],
    );
    assert_builtin_template(
        &domain_rules::kinds::rust::RustRegistry,
        "rust",
        &["rust_alloc_fn_Box::new", "rust_cleanup_fn_std::mem::forget"],
    );
    assert_builtin_template(
        &domain_rules::kinds::typescript::TypeScriptRegistry,
        "typescript",
        &["ts_alloc_fn_open", "ts_react_hook_useEffect"],
    );
}
