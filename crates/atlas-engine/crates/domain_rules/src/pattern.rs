//! Pattern matching — match targets against domain rules by pattern kind.

use super::types::{DomainRule, PatternKind};

/// Match a target string against a domain rule's pattern.
pub fn match_pattern(rule: &DomainRule, target: &str) -> bool {
    let kind = match PatternKind::from_str(&rule.pattern_kind) {
        Some(k) => k,
        None => return false,
    };
    match kind {
        PatternKind::Exact => rule.pattern == target,
        PatternKind::Prefix => target.starts_with(&rule.pattern),
        PatternKind::Suffix => target.ends_with(&rule.pattern),
        PatternKind::Glob => glob_match(&rule.pattern, target),
        PatternKind::Regex => match regex::Regex::new(&rule.pattern) {
            Ok(re) => re.is_match(target),
            Err(_) => rule.pattern == target,
        },
    }
}

/// Simple glob matching: `*` matches any sequence of chars, `?` matches any single char.
fn glob_match(pattern: &str, target: &str) -> bool {
    let pat = pattern.as_bytes();
    let tgt = target.as_bytes();
    let plen = pat.len();
    let tlen = tgt.len();

    // State: (pat_idx, tgt_idx)
    let mut star_idx: Option<usize> = None;
    let mut tgt_star_idx: usize = 0;
    let mut pi: usize = 0;
    let mut ti: usize = 0;

    while pi < plen || ti < tlen {
        if pi < plen {
            match pat[pi] {
                b'*' => {
                    star_idx = Some(pi);
                    tgt_star_idx = ti;
                    pi += 1;
                    continue;
                }
                b'?' => {
                    if ti < tlen {
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
                c => {
                    if ti < tlen && c == tgt[ti] {
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
            }
        }

        if let Some(si) = star_idx {
            pi = si + 1;
            tgt_star_idx += 1;
            ti = tgt_star_idx;
        } else {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rule(pattern: &str, pattern_kind: &str) -> DomainRule {
        DomainRule {
            id: "test".into(),
            language: "c".into(),
            rule_kind: "free_fn".into(),
            pattern: pattern.into(),
            pattern_kind: pattern_kind.into(),
            meta: None,
            meta_version: 1,
            source: "user".into(),
            status: "enabled".into(),
            confidence: 1.0,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn test_exact_match() {
        let rule = make_rule("free", "exact");
        assert!(match_pattern(&rule, "free"));
        assert!(!match_pattern(&rule, "my_free"));
    }

    #[test]
    fn test_prefix_match() {
        let rule = make_rule("my_", "prefix");
        assert!(match_pattern(&rule, "my_free"));
        assert!(match_pattern(&rule, "my_alloc"));
        assert!(!match_pattern(&rule, "not_mine"));
    }

    #[test]
    fn test_suffix_match() {
        let rule = make_rule("_free", "suffix");
        assert!(match_pattern(&rule, "my_free"));
        assert!(match_pattern(&rule, "safe_free"));
        assert!(!match_pattern(&rule, "free_ptr"));
    }

    #[test]
    fn test_glob_match() {
        let rule = make_rule("my_*_fn", "glob");
        assert!(match_pattern(&rule, "my_free_fn"));
        assert!(match_pattern(&rule, "my_alloc_fn"));
        assert!(!match_pattern(&rule, "free_my_fn"));
    }

    #[test]
    fn test_glob_question_mark() {
        let rule = make_rule("alloc_?", "glob");
        assert!(match_pattern(&rule, "alloc_x"));
        assert!(match_pattern(&rule, "alloc_1"));
        assert!(!match_pattern(&rule, "alloc_xx"));
    }

    #[test]
    fn test_regex_match() {
        let rule = make_rule(r"^(free|delete|release)_", "regex");
        assert!(match_pattern(&rule, "free_buffer"));
        assert!(match_pattern(&rule, "delete_ptr"));
        assert!(match_pattern(&rule, "release_resource"));
        assert!(!match_pattern(&rule, "not_free"));
    }

    #[test]
    fn test_invalid_regex_falls_back_to_exact() {
        let rule = make_rule("[invalid", "regex");
        assert!(!match_pattern(&rule, "anything"));
        assert!(match_pattern(&rule, "[invalid"));
    }
}
