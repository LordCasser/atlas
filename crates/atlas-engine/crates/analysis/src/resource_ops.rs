//! Resource operation patterns — per-language alloc/free/open/close signatures.
//!
//! These patterns replace the hardcoded C/C++ alloc/free lists in the CFG builder
//! and extend branch_diff to work with any language via DataFlow-based enrichment.

use types::enums::Language;

// ---------------------------------------------------------------------------
// CalleeMatcher — flexible name matching for resource operations
// ---------------------------------------------------------------------------

/// How to match a callee name.
#[derive(Debug, Clone)]
pub enum CalleeMatcher {
    /// Exact match (case-insensitive)
    Exact(String),
    /// Prefix match (e.g., "curl_")
    Prefix(String),
    /// Suffix match (e.g., "_free")
    Suffix(String),
    /// Case-insensitive substring match
    Contains(String),
}

impl CalleeMatcher {
    pub fn matches(&self, callee: &str) -> bool {
        let lower = callee.to_lowercase();
        match self {
            Self::Exact(s) => lower == s.to_lowercase(),
            Self::Prefix(s) => lower.starts_with(&s.to_lowercase()),
            Self::Suffix(s) => lower.ends_with(&s.to_lowercase()),
            Self::Contains(s) => lower.contains(&s.to_lowercase()),
        }
    }
}

// ---------------------------------------------------------------------------
// ResourceOpKind / ResourceOpPattern
// ---------------------------------------------------------------------------

/// What a resource operation does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceOpKind {
    /// Creates/allocates a new resource (e.g., malloc, fopen, new, sql.Open)
    Produce,
    /// Consumes/frees a resource (e.g., free, fclose, delete, .close())
    Consume,
}

/// A single resource operation pattern for a language.
#[derive(Debug, Clone)]
pub struct ResourceOpPattern {
    pub kind: ResourceOpKind,
    pub matcher: CalleeMatcher,
    /// For Consume: which parameter index is the resource (0-based).
    /// For Produce: not used (return value is always parameter index 0 conceptually).
    pub resource_param_index: usize,
}

impl ResourceOpPattern {
    pub fn new(kind: ResourceOpKind, matcher: CalleeMatcher, resource_param_index: usize) -> Self {
        Self {
            kind,
            matcher,
            resource_param_index,
        }
    }
}

// ---------------------------------------------------------------------------
// ResourceOpConfig — per-language resource operation configuration
// ---------------------------------------------------------------------------

/// Resource operation configuration per language.
#[derive(Debug, Clone)]
pub struct ResourceOpConfig {
    pub language: Option<Language>, // None = default/fallback
    pub producers: Vec<ResourceOpPattern>,
    pub consumers: Vec<ResourceOpPattern>,
}

impl ResourceOpConfig {
    /// Return built-in defaults for each supported language.
    pub fn default_for(lang: Language) -> Self {
        match lang {
            Language::C | Language::Cpp => Self::default_c_like(),
            Language::TypeScript | Language::JavaScript => Self::default_ts_js(),
            Language::Python => Self::default_python(),
            Language::Java => Self::default_java(),
            Language::Go => Self::default_go(),
            Language::Rust => Self::default_rust(),
            Language::CSharp => Self::default_csharp(),
            Language::Php => Self::default_php(),
            Language::Ruby => Self::default_ruby(),
            Language::Kotlin => Self::default_kotlin(),
            Language::ArkTS | Language::Cangjie => Self::default_minimal(lang),
        }
    }

    /// C and C++ — rich built-in alloc/free patterns.
    fn default_c_like() -> Self {
        use CalleeMatcher::{Exact, Prefix, Suffix};
        let language = None; // covers both C and Cpp
        // Producers — functions that return an allocated resource handle
        let producers = vec![
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("malloc".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("calloc".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("realloc".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("strdup".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("strndup".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("fopen".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("asprintf".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("aprintf".into()), 0),
            ResourceOpPattern::new(
                ResourceOpKind::Produce,
                Exact("Curl_copy_header_value".into()),
                0,
            ),
            ResourceOpPattern::new(ResourceOpKind::Produce, Prefix("curl_copy_".into()), 0),
        ];
        // Consumers — functions that take a resource handle as argument
        let consumers = vec![
            ResourceOpPattern::new(ResourceOpKind::Consume, Exact("free".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Exact("fclose".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Exact("closedir".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Exact("safefree".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Exact("Curl_safefree".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix("_free".into()), 0),
        ];
        Self {
            language,
            producers,
            consumers,
        }
    }

    /// TypeScript / JavaScript — .dispose(), .close(), .destroy() patterns.
    fn default_ts_js() -> Self {
        use CalleeMatcher::{Exact, Suffix};
        let language = None;
        let producers = vec![
            // new X() is implicit; open(), createConnection() etc.
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("open".into()), 0),
            ResourceOpPattern::new(
                ResourceOpKind::Produce,
                Suffix("createConnection".into()),
                0,
            ),
            ResourceOpPattern::new(ResourceOpKind::Produce, Suffix("openConnection".into()), 0),
        ];
        let consumers = vec![
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".dispose".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".close".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".destroy".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".release".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Exact("clearTimeout".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Exact("clearInterval".into()), 0),
        ];
        Self {
            language,
            producers,
            consumers,
        }
    }

    /// Python — open()/close() patterns, __del__, context managers.
    fn default_python() -> Self {
        use CalleeMatcher::{Exact, Suffix};
        let language = None;
        let producers = vec![
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("open".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Suffix("connect".into()), 0),
        ];
        let consumers = vec![
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".close".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".dispose".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".release".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Exact("os.close".into()), 0),
        ];
        Self {
            language,
            producers,
            consumers,
        }
    }

    /// Java — try-with-resources, .close(), .dispose().
    fn default_java() -> Self {
        use CalleeMatcher::Suffix;
        let language = None;
        let producers = vec![
            ResourceOpPattern::new(ResourceOpKind::Produce, Suffix("openConnection".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Suffix("openStream".into()), 0),
        ];
        let consumers = vec![
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".close".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".dispose".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".destroy".into()), 0),
        ];
        Self {
            language,
            producers,
            consumers,
        }
    }

    /// Go — os.Open, sql.Open, .Close().
    fn default_go() -> Self {
        use CalleeMatcher::{Contains, Suffix};
        let language = None;
        let producers = vec![ResourceOpPattern::new(
            ResourceOpKind::Produce,
            Contains("Open".into()),
            0,
        )];
        let consumers = vec![ResourceOpPattern::new(
            ResourceOpKind::Consume,
            Suffix(".Close".into()),
            0,
        )];
        Self {
            language,
            producers,
            consumers,
        }
    }

    /// Rust — Box::new, Arc::new producers; drop() is implicit so skip.
    fn default_rust() -> Self {
        use CalleeMatcher::Contains;
        let language = None;
        let producers = vec![ResourceOpPattern::new(
            ResourceOpKind::Produce,
            Contains("::new".into()),
            0,
        )];
        let consumers = Vec::new(); // Rust's Drop is compiler-generated
        Self {
            language,
            producers,
            consumers,
        }
    }

    /// C# — .Dispose(), using statements.
    fn default_csharp() -> Self {
        use CalleeMatcher::Suffix;
        let language = None;
        let producers = vec![
            ResourceOpPattern::new(ResourceOpKind::Produce, Suffix("OpenConnection".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Suffix("OpenStream".into()), 0),
        ];
        let consumers = vec![
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".Dispose".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".Close".into()), 0),
        ];
        Self {
            language,
            producers,
            consumers,
        }
    }

    /// PHP.
    fn default_php() -> Self {
        use CalleeMatcher::{Exact, Suffix};
        let language = None;
        let producers = vec![
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("fopen".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Suffix("connect".into()), 0),
        ];
        let consumers = vec![
            ResourceOpPattern::new(ResourceOpKind::Consume, Exact("fclose".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix("close".into()), 0),
        ];
        Self {
            language,
            producers,
            consumers,
        }
    }

    /// Ruby.
    fn default_ruby() -> Self {
        use CalleeMatcher::Suffix;
        let language = None;
        let producers = vec![
            ResourceOpPattern::new(ResourceOpKind::Produce, Suffix(".open".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Suffix(".new".into()), 0),
        ];
        let consumers = vec![
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".close".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".dispose".into()), 0),
        ];
        Self {
            language,
            producers,
            consumers,
        }
    }

    /// Kotlin.
    fn default_kotlin() -> Self {
        use CalleeMatcher::Suffix;
        let language = None;
        let producers = vec![ResourceOpPattern::new(
            ResourceOpKind::Produce,
            Suffix("openConnection".into()),
            0,
        )];
        let consumers = vec![
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".close".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Suffix(".dispose".into()), 0),
        ];
        Self {
            language,
            producers,
            consumers,
        }
    }

    /// Minimal fallback for experimental languages.
    fn default_minimal(lang: Language) -> Self {
        Self {
            language: Some(lang),
            producers: Vec::new(),
            consumers: Vec::new(),
        }
    }

    // ── Query helpers ──────────────────────────────────────────────────────

    /// Check if a callee name matches any producer pattern.
    pub fn is_producer(&self, callee: &str) -> bool {
        self.producers.iter().any(|p| p.matcher.matches(callee))
    }

    /// Check if a callee name matches any consumer pattern, returns param index if so.
    pub fn is_consumer(&self, callee: &str) -> Option<usize> {
        self.consumers
            .iter()
            .find(|p| p.matcher.matches(callee))
            .map(|p| p.resource_param_index)
    }

    /// Merge another config into self (non-language-specific fields only).
    pub fn merge_defaults(&mut self) {
        let default = Self::default_c_like();
        // Add any default producers/consumers not already present
        for p in default.producers {
            let already = self.producers.iter().any(|existing| {
                std::mem::discriminant(&existing.matcher) == std::mem::discriminant(&p.matcher)
            });
            if !already {
                self.producers.push(p);
            }
        }
        for c in default.consumers {
            let already = self.consumers.iter().any(|existing| {
                std::mem::discriminant(&existing.matcher) == std::mem::discriminant(&c.matcher)
            });
            if !already {
                self.consumers.push(c);
            }
        }
    }
}

// ── OwnershipContract impl ─────────────────────────────────────────────────

use types::effects::{
    ConsumptionContract, ConsumptionStyle, OwnershipContract, ResourceLocator, ReturnContract,
};

impl OwnershipContract for ResourceOpConfig {
    fn classify_return(&self, callee: &str) -> Option<ReturnContract> {
        // 匹配 producer 模式 → NewOwned
        if self.producers.iter().any(|p| p.matcher.matches(callee)) {
            return Some(ReturnContract::NewOwned);
        }
        // C 特有的 realloc 语义：maybe owned
        let lower = callee.to_lowercase();
        if lower == "realloc" {
            return Some(ReturnContract::MaybeOwned);
        }
        None
    }

    fn classify_consumption(&self, callee: &str) -> Option<ConsumptionContract> {
        // 匹配 consumer 模式
        self.consumers
            .iter()
            .find(|c| c.matcher.matches(callee))
            .map(|pattern| {
                // 根据 callee 名称判断消费风格
                let style = detect_consumption_style(callee);
                ConsumptionContract {
                    resource: ResourceLocator::Argument {
                        index: pattern.resource_param_index,
                    },
                    style,
                    confidence: 0.85,
                }
            })
    }
}

/// 从 callee 名称推断消费/释放的语法风格。
fn detect_consumption_style(callee: &str) -> ConsumptionStyle {
    // Method call: 包含 ".close" / ".dispose" / ".destroy" / ".release" 模式
    if callee.contains(".close")
        || callee.contains(".Close")
        || callee.contains(".dispose")
        || callee.contains(".Dispose")
        || callee.contains(".destroy")
        || callee.contains(".Destroy")
        || callee.contains(".release")
        || callee.contains(".Release")
    {
        return ConsumptionStyle::MethodCall;
    }
    // Go defer: 前缀 "defer " 或包含 "defer "
    if callee.starts_with("defer ") || callee.contains("defer ") {
        return ConsumptionStyle::Deferred;
    }
    // 默认：free 函数样式
    ConsumptionStyle::ExplicitCall
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_callee_matcher_exact() {
        let m = CalleeMatcher::Exact("free".into());
        assert!(m.matches("free"));
        assert!(m.matches("Free"));
        assert!(m.matches("FREE"));
        assert!(!m.matches("safefree"));
    }

    #[test]
    fn test_callee_matcher_prefix() {
        let m = CalleeMatcher::Prefix("curl_".into());
        assert!(m.matches("curl_copy_header_value"));
        assert!(m.matches("Curl_safefree"));
        assert!(!m.matches("free"));
    }

    #[test]
    fn test_callee_matcher_suffix() {
        let m = CalleeMatcher::Suffix("_free".into());
        assert!(m.matches("obj_free"));
        assert!(m.matches("str_free"));
        assert!(!m.matches("free"));
        assert!(!m.matches("safefree")); // no underscore before "free"
        assert!(!m.matches("Curl_safefree"));
    }

    #[test]
    fn test_callee_matcher_contains() {
        let m = CalleeMatcher::Contains("Open".into());
        assert!(m.matches("os.Open"));
        assert!(m.matches("sql.Open"));
        assert!(m.matches("openConnection"));
        assert!(!m.matches("close"));
    }

    #[test]
    fn test_c_config_produces() {
        let config = ResourceOpConfig::default_for(Language::C);
        assert!(config.is_producer("malloc"));
        assert!(config.is_producer("Curl_copy_header_value"));
        assert!(config.is_producer("curl_copy_something"));
        assert!(!config.is_producer("free"));
    }

    #[test]
    fn test_c_config_consumes() {
        let config = ResourceOpConfig::default_for(Language::C);
        assert_eq!(config.is_consumer("free"), Some(0));
        assert_eq!(config.is_consumer("Curl_safefree"), Some(0));
        assert_eq!(config.is_consumer("safefree"), Some(0));
        assert_eq!(config.is_consumer("malloc"), None);
    }
}
