//! Taint rule loader: YAML-based rule definitions.
//!
//! # Architecture
//!
//! Rules are defined in YAML format and loaded from two sources:
//! 1. **Embedded defaults** — language-specific built-in rules for TS/JS and Python
//! 2. **User rules** — `.atlas/rules/*.yaml` files in the project root (override defaults)
//!
//! Rule matching uses `symbol_pattern` (substring match on qualified name) and
//! `access_path_pattern` (regex match on field access chain like `req.body.user.name`).

use crate::types::taint::TaintRule;

#[cfg(test)]
use crate::types::taint::{Severity, TaintRuleKind};
use crate::types::enums::Language;

/// Embedded default taint rules for TypeScript/JavaScript.
const DEFAULT_RULES_TS: &str = r#"
# ── Sources ──────────────────────────────────────────────────────────────────
- id: ts.req.query
  language: typescript
  kind: source
  symbol_pattern: "query"
  callee: "request"
  severity: high
- id: ts.req.body
  language: typescript
  kind: source
  symbol_pattern: "body"
  callee: "request"
  severity: high
- id: ts.req.params
  language: typescript
  kind: source
  symbol_pattern: "params"
  callee: "request"
  severity: high
- id: ts.req.cookies
  language: typescript
  kind: source
  symbol_pattern: "cookies"
  callee: "request"
  severity: medium
- id: ts.req.headers
  language: typescript
  kind: source
  symbol_pattern: "headers"
  callee: "request"
  severity: medium
- id: ts.location.hash
  language: typescript
  kind: source
  symbol_pattern: "hash"
  callee: "location"
  severity: low
- id: ts.location.search
  language: typescript
  kind: source
  symbol_pattern: "search"
  callee: "location"
  severity: low
- id: ts.document.cookie
  language: typescript
  kind: source
  symbol_pattern: "cookie"
  callee: "document"
  severity: low
- id: ts.localstorage.getItem
  language: typescript
  kind: source
  symbol_pattern: "getItem"
  callee: "localStorage"
  severity: low

# ── Sinks ────────────────────────────────────────────────────────────────────
- id: ts.eval
  language: typescript
  kind: sink
  symbol_pattern: "eval"
  severity: critical
- id: ts.function
  language: typescript
  kind: sink
  symbol_pattern: "Function"
  severity: critical
- id: ts.innerHTML
  language: typescript
  kind: sink
  symbol_pattern: "innerHTML"
  severity: high
- id: ts.outerHTML
  language: typescript
  kind: sink
  symbol_pattern: "outerHTML"
  severity: high
- id: ts.document.write
  language: typescript
  kind: sink
  symbol_pattern: "write"
  callee: "document"
  severity: medium
- id: ts.child_process.exec
  language: typescript
  kind: sink
  symbol_pattern: "exec"
  callee: "child_process"
  severity: critical
- id: ts.child_process.spawn
  language: typescript
  kind: sink
  symbol_pattern: "spawn"
  callee: "child_process"
  severity: critical
- id: ts.child_process.execSync
  language: typescript
  kind: sink
  symbol_pattern: "execSync"
  callee: "child_process"
  severity: critical
- id: ts.child_process.spawnSync
  language: typescript
  kind: sink
  symbol_pattern: "spawnSync"
  callee: "child_process"
  severity: critical
- id: ts.fs.readFile
  language: typescript
  kind: sink
  symbol_pattern: "readFile"
  callee: "fs"
  severity: low
- id: ts.fs.writeFile
  language: typescript
  kind: sink
  symbol_pattern: "writeFile"
  callee: "fs"
  severity: low

# ── Sanitizers ───────────────────────────────────────────────────────────────
- id: ts.sanitize
  language: typescript
  kind: sanitizer
  symbol_pattern: "sanitize"
  severity: info
- id: ts.escapeHtml
  language: typescript
  kind: sanitizer
  symbol_pattern: "escapeHtml"
  severity: info
- id: ts.encodeURIComponent
  language: typescript
  kind: sanitizer
  symbol_pattern: "encodeURIComponent"
  severity: info
- id: ts.parseInt
  language: typescript
  kind: sanitizer
  symbol_pattern: "parseInt"
  severity: info
"#;

/// Embedded default taint rules for Python.
const DEFAULT_RULES_PYTHON: &str = r#"
# ── Sources ──────────────────────────────────────────────────────────────────
- id: py.request.args
  language: python
  kind: source
  symbol_pattern: "args"
  callee: "request"
  severity: high
- id: py.request.form
  language: python
  kind: source
  symbol_pattern: "form"
  callee: "request"
  severity: high
- id: py.request.json
  language: python
  kind: source
  symbol_pattern: "json"
  callee: "request"
  severity: high
- id: py.request.cookies
  language: python
  kind: source
  symbol_pattern: "cookies"
  callee: "request"
  severity: medium
- id: py.sys.argv
  language: python
  kind: source
  symbol_pattern: "argv"
  callee: "sys"
  severity: low
- id: py.os.environ
  language: python
  kind: source
  symbol_pattern: "environ"
  callee: "os"
  severity: low

# ── Sinks ────────────────────────────────────────────────────────────────────
- id: py.os.system
  language: python
  kind: sink
  symbol_pattern: "system"
  callee: "os"
  severity: critical
- id: py.subprocess.call
  language: python
  kind: sink
  symbol_pattern: "call"
  callee: "subprocess"
  severity: critical
- id: py.subprocess.Popen
  language: python
  kind: sink
  symbol_pattern: "Popen"
  callee: "subprocess"
  severity: critical
- id: py.exec
  language: python
  kind: sink
  symbol_pattern: "exec"
  severity: critical
- id: py.eval
  language: python
  kind: sink
  symbol_pattern: "eval"
  severity: critical
- id: py.pickle.loads
  language: python
  kind: sink
  symbol_pattern: "loads"
  callee: "pickle"
  severity: high
- id: py.yaml.load
  language: python
  kind: sink
  symbol_pattern: "load"
  callee: "yaml"
  severity: high
- id: py.open
  language: python
  kind: sink
  symbol_pattern: "open"
  severity: low

# ── Sanitizers ───────────────────────────────────────────────────────────────
- id: py.html.escape
  language: python
  kind: sanitizer
  symbol_pattern: "html.escape"
  severity: info
- id: py.urllib.quote
  language: python
  kind: sanitizer
  symbol_pattern: "quote"
  callee: "urllib.parse"
  severity: info
- id: py.int
  language: python
  kind: sanitizer
  symbol_pattern: "int"
  severity: info
"#;

// ── TaintRuleLoader ─────────────────────────────────────────────────────────

/// Loads taint rules from embedded defaults and optional user YAML files.
pub struct TaintRuleLoader;

impl TaintRuleLoader {
    /// Load all rules: default rules first, then user overrides.
    ///
    /// Default rules are always loaded for configured languages.
    /// User rules (`.atlas/rules/*.yaml`) override or extend defaults.
    pub fn load_all(
        languages: &[Language],
        user_rules_dir: Option<&std::path::Path>,
    ) -> Vec<TaintRule> {
        let mut rules = Self::load_defaults(languages);

        if let Some(dir) = user_rules_dir {
            if let Ok(user_rules) = Self::load_user_rules(dir) {
                // User rules override defaults by matching (language, kind, symbol_pattern, callee)
                for user_rule in user_rules {
                    rules.retain(|r| {
                        !(r.language == user_rule.language
                            && r.kind == user_rule.kind
                            && r.symbol_pattern == user_rule.symbol_pattern
                            && r.callee == user_rule.callee)
                    });
                    rules.push(user_rule);
                }
            }
        }

        rules
    }

    /// Load default rules for the given languages.
    pub fn load_defaults(languages: &[Language]) -> Vec<TaintRule> {
        let mut rules = Vec::new();

        for lang in languages {
            let yaml = match lang {
                Language::TypeScript | Language::JavaScript => DEFAULT_RULES_TS,
                Language::Python => DEFAULT_RULES_PYTHON,
                _ => continue, // No default rules for other languages yet
            };

            if let Ok(lang_rules) = serde_yaml::from_str::<Vec<TaintRule>>(yaml) {
                rules.extend(lang_rules);
            }
        }

        rules
    }

    /// Load user-defined rules from `.atlas/rules/*.yaml`.
    pub fn load_user_rules(dir: &std::path::Path) -> anyhow::Result<Vec<TaintRule>> {
        let mut rules = Vec::new();

        if !dir.exists() || !dir.is_dir() {
            return Ok(rules);
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "yaml" || ext == "yml") {
                let content = std::fs::read_to_string(&path)?;
                let file_rules: Vec<TaintRule> = serde_yaml::from_str(&content)?;
                rules.extend(file_rules);
            }
        }

        Ok(rules)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_ts_defaults() {
        let rules = TaintRuleLoader::load_defaults(&[Language::TypeScript]);
        let sources: Vec<_> = rules.iter().filter(|r| r.kind == TaintRuleKind::Source).collect();
        let sinks: Vec<_> = rules.iter().filter(|r| r.kind == TaintRuleKind::Sink).collect();
        let sanitizers: Vec<_> = rules.iter().filter(|r| r.kind == TaintRuleKind::Sanitizer).collect();

        assert!(!sources.is_empty(), "Should have TS source rules");
        assert!(!sinks.is_empty(), "Should have TS sink rules");
        assert!(!sanitizers.is_empty(), "Should have TS sanitizer rules");
        for r in &sources {
            assert_eq!(r.language, Some(Language::TypeScript));
        }
    }

    #[test]
    fn test_load_python_defaults() {
        let rules = TaintRuleLoader::load_defaults(&[Language::Python]);
        assert!(!rules.is_empty(), "Should have Python rules");
        for r in &rules {
            assert_eq!(r.language, Some(Language::Python));
        }
    }

    #[test]
    fn test_rule_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let rules_dir = dir.path().join(".atlas").join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();

        let user_yaml = r#"
- id: ts.req.query
  language: typescript
  kind: source
  symbol_pattern: "query"
  callee: "request"
  severity: critical
"#;
        std::fs::write(rules_dir.join("custom.yaml"), user_yaml).unwrap();

        let rules = TaintRuleLoader::load_all(
            &[Language::TypeScript],
            Some(&rules_dir),
        );

        // The overridden rule should have severity "critical" (not "high" as default)
        let query_rule = rules.iter().find(|r| {
            r.kind == TaintRuleKind::Source
                && r.symbol_pattern.as_deref() == Some("query")
                && r.callee.as_deref() == Some("request")
        }).unwrap();
        assert_eq!(query_rule.severity, Severity::Critical);
    }
}
