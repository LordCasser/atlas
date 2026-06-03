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
    /// Matches any callee (wildcard)
    Wildcard,
}

impl CalleeMatcher {
    pub fn matches(&self, callee: &str) -> bool {
        let lower = callee.to_lowercase();
        match self {
            Self::Exact(s) => lower == s.to_lowercase(),
            Self::Prefix(s) => lower.starts_with(&s.to_lowercase()),
            Self::Suffix(s) => lower.ends_with(&s.to_lowercase()),
            Self::Contains(s) => lower.contains(&s.to_lowercase()),
            Self::Wildcard => true,
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
    /// Patterns for resource escape (goroutine, thread, global, etc.)
    pub escapes: Vec<ResourceOpPattern>,
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
            escapes: vec![],
        }
    }

    /// TypeScript / JavaScript — .dispose(), .close(), .destroy() patterns.
    ///
    /// Limitation: cleanup return analysis for React hooks (useEffect
    /// returning a destructor function) is not yet implemented.  useEffect
    /// is classified as MaybeOwned to reflect the subscription semantic;
    /// the returned cleanup function is not currently tracked.
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
            // Node.js stream and server factories
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("createReadStream".into()), 0),
            ResourceOpPattern::new(
                ResourceOpKind::Produce,
                Exact("createWriteStream".into()),
                0,
            ),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("createServer".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("createClient".into()), 0),
            // React hooks (see limitation above)
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("useEffect".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("useMemo".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("useCallback".into()), 0),
            // Timer factories
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("setTimeout".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("setInterval".into()), 0),
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
            escapes: vec![],
        }
    }

    /// Python — open()/close() patterns, __del__, context managers.
    ///
    /// Limitation: implicit context-manager lifecycle (Python `with open`)
    /// is handled by ScopeExitAnalyzer (Free-at-Exit pass), not by these
    /// function-level patterns.
    fn default_python() -> Self {
        use CalleeMatcher::{Exact, Suffix};
        let language = None;
        let producers = vec![
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("open".into()), 0),
            // Standard-library resource producers
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("sqlite3.connect".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("socket.socket".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("requests.Session".into()), 0),
            // Catch-all suffix for custom connect-like factories
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
            escapes: vec![],
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
            escapes: vec![],
        }
    }

    /// Go — os.Open, sql.Open, net.Dial; .Close() consumers.
    fn default_go() -> Self {
        use CalleeMatcher::{Exact, Suffix};
        let language = None;
        let producers = vec![
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("os.Open".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("os.Create".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("sql.Open".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("net.Dial".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("os.OpenFile".into()), 0),
        ];
        let consumers = vec![ResourceOpPattern::new(
            ResourceOpKind::Consume,
            Suffix(".Close()".into()),
            0,
        )];
        // Go escape is handled by CallContext::GoGoroutine, not by explicit patterns
        Self {
            language,
            producers,
            consumers,
            escapes: vec![],
        }
    }

    /// Rust — Box::new, Vec::new, Arc::new, Rc::new producers; drop() / forget() consumers.
    fn default_rust() -> Self {
        use CalleeMatcher::Exact;
        let language = None;
        let producers = vec![
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("Box::new".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("Vec::new".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("Arc::new".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("Rc::new".into()), 0),
            ResourceOpPattern::new(
                ResourceOpKind::Produce,
                Exact("std::sync::Arc::new".into()),
                0,
            ),
            ResourceOpPattern::new(ResourceOpKind::Produce, Exact("std::rc::Rc::new".into()), 0),
        ];
        let consumers = vec![
            ResourceOpPattern::new(ResourceOpKind::Consume, Exact("drop".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Exact("std::mem::drop".into()), 0),
            ResourceOpPattern::new(ResourceOpKind::Consume, Exact("std::mem::forget".into()), 0),
        ];
        // mem::forget causes escape (not just consumption)
        let escapes = vec![ResourceOpPattern::new(
            ResourceOpKind::Consume,
            Exact("std::mem::forget".into()),
            0,
        )];
        Self {
            language,
            producers,
            consumers,
            escapes,
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
            escapes: vec![],
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
            escapes: vec![],
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
            escapes: vec![],
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
            escapes: vec![],
        }
    }

    /// Minimal fallback for experimental languages.
    fn default_minimal(lang: Language) -> Self {
        Self {
            language: Some(lang),
            producers: Vec::new(),
            consumers: Vec::new(),
            escapes: vec![],
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
    ConsumptionContract, ConsumptionStyle, EscapeTarget, OwnershipContract, ResourceLocator,
    ReturnContract,
};
use types::enums::CallContext;

impl OwnershipContract for ResourceOpConfig {
    fn classify_return(&self, callee: &str) -> Option<ReturnContract> {
        // Special cases: MaybeOwned (check before generic producer match).
        // React useEffect returns a subscription resource that may or may
        // not own — cleanup return analysis not yet implemented.
        // C realloc semantics: realloc(NULL, N) allocates, realloc(ptr, N) may reuse.
        let lower = callee.to_lowercase();
        if lower == "useeffect" || lower == "realloc" {
            return Some(ReturnContract::MaybeOwned);
        }
        // 匹配 producer 模式 → NewOwned
        if self.producers.iter().any(|p| p.matcher.matches(callee)) {
            return Some(ReturnContract::NewOwned);
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

    fn classify_escape(&self, callee: &str, context: CallContext) -> Option<EscapeTarget> {
        match context {
            CallContext::GoGoroutine => Some(EscapeTarget::Thread),
            _ => {
                // Check explicit escape patterns (e.g., std::mem::forget)
                for pattern in &self.escapes {
                    if pattern.matcher.matches(callee) {
                        return Some(EscapeTarget::Thread);
                    }
                }
                None
            }
        }
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

    // ── Go tests ───────────────────────────────────────────────────────────

    #[cfg(feature = "go")]
    #[test]
    fn test_go_config_produces() {
        let config = ResourceOpConfig::default_for(Language::Go);
        // Exact producers should match
        assert!(config.is_producer("os.Open"));
        assert!(config.is_producer("os.Create"));
        assert!(config.is_producer("sql.Open"));
        assert!(config.is_producer("net.Dial"));
        assert!(config.is_producer("os.OpenFile"));
        // Suffix ".Close()" consumers should NOT be producers
        assert!(!config.is_producer("file.Close()"));
        // Unrelated names should not match
        assert!(!config.is_producer("close"));
        assert!(!config.is_producer("free"));
    }

    #[cfg(feature = "go")]
    #[test]
    fn test_go_config_consumes() {
        let config = ResourceOpConfig::default_for(Language::Go);
        // Suffix ".Close()" should match method-style and defer-style closes
        assert_eq!(config.is_consumer("file.Close()"), Some(0));
        assert_eq!(config.is_consumer("conn.Close()"), Some(0));
        assert_eq!(config.is_consumer("resp.Body.Close()"), Some(0));
        // Verify Close is NOT detected without parentheses suffix
        assert_eq!(config.is_consumer("Close"), None);
        // Producers should NOT be consumers
        assert_eq!(config.is_consumer("os.Open"), None);
        assert_eq!(config.is_consumer("sql.Open"), None);
    }

    #[cfg(feature = "go")]
    #[test]
    fn test_go_config_classify() {
        let config = ResourceOpConfig::default_for(Language::Go);
        // classify_return uses producer patterns
        assert_eq!(
            config.classify_return("os.Open"),
            Some(ReturnContract::NewOwned)
        );
        assert_eq!(
            config.classify_return("sql.Open"),
            Some(ReturnContract::NewOwned)
        );
        assert_eq!(config.classify_return("file.Close()"), None);
        // classify_consumption uses consumer patterns
        let cc = config.classify_consumption("file.Close()");
        assert!(cc.is_some());
        let contract = cc.unwrap();
        assert_eq!(contract.style, ConsumptionStyle::MethodCall);
        // classify_consumption should return None for non-consumers
        assert!(config.classify_consumption("os.Open").is_none());
    }

    // ── Rust tests ─────────────────────────────────────────────────────────

    #[cfg(feature = "rust")]
    #[test]
    fn test_rust_config_produces() {
        let config = ResourceOpConfig::default_for(Language::Rust);
        // Exact producers
        assert!(config.is_producer("Box::new"));
        assert!(config.is_producer("Vec::new"));
        assert!(config.is_producer("Arc::new"));
        assert!(config.is_producer("Rc::new"));
        assert!(config.is_producer("std::sync::Arc::new"));
        assert!(config.is_producer("std::rc::Rc::new"));
        // Consumers should NOT be producers
        assert!(!config.is_producer("drop"));
        assert!(!config.is_producer("std::mem::drop"));
        assert!(!config.is_producer("std::mem::forget"));
        // Unrelated names
        assert!(!config.is_producer("free"));
        assert!(!config.is_producer("new")); // "::new" pattern was removed
    }

    #[cfg(feature = "rust")]
    #[test]
    fn test_rust_config_consumes() {
        let config = ResourceOpConfig::default_for(Language::Rust);
        // Exact consumers
        assert_eq!(config.is_consumer("drop"), Some(0));
        assert_eq!(config.is_consumer("std::mem::drop"), Some(0));
        assert_eq!(config.is_consumer("std::mem::forget"), Some(0));
        // Producers should NOT be consumers
        assert_eq!(config.is_consumer("Box::new"), None);
        assert_eq!(config.is_consumer("Arc::new"), None);
    }

    #[cfg(feature = "rust")]
    #[test]
    fn test_rust_config_classify() {
        let config = ResourceOpConfig::default_for(Language::Rust);
        // classify_return
        assert_eq!(
            config.classify_return("Box::new"),
            Some(ReturnContract::NewOwned)
        );
        assert_eq!(
            config.classify_return("Arc::new"),
            Some(ReturnContract::NewOwned)
        );
        assert_eq!(config.classify_return("drop"), None);
        // classify_consumption
        let cc = config.classify_consumption("drop");
        assert!(cc.is_some());
        let contract = cc.unwrap();
        assert_eq!(contract.style, ConsumptionStyle::ExplicitCall);
        // std::mem::forget is an escape consumer
        assert!(config.classify_consumption("std::mem::forget").is_some());
        // Producers should not consume
        assert!(config.classify_consumption("Box::new").is_none());
    }

    // ── Part C: Go goroutine escape tests ──────────────────────────────────

    #[cfg(feature = "go")]
    #[test]
    fn test_go_goroutine_escape() {
        let config = ResourceOpConfig::default_for(Language::Go);
        let result = config.classify_escape("myHandler", CallContext::GoGoroutine);
        assert_eq!(result, Some(EscapeTarget::Thread));
    }

    #[cfg(feature = "go")]
    #[test]
    fn test_go_normal_call_no_escape() {
        let config = ResourceOpConfig::default_for(Language::Go);
        let result = config.classify_escape("fmt.Println", CallContext::None);
        assert_eq!(result, None);
    }

    // ── Part C: Rust mem::forget escape test ───────────────────────────────

    #[cfg(feature = "rust")]
    #[test]
    fn test_rust_mem_forget_escape() {
        let config = ResourceOpConfig::default_for(Language::Rust);
        let result = config.classify_escape("std::mem::forget", CallContext::None);
        assert_eq!(result, Some(EscapeTarget::Thread));
    }

    #[cfg(feature = "rust")]
    #[test]
    fn test_rust_drop_no_escape() {
        let config = ResourceOpConfig::default_for(Language::Rust);
        let result = config.classify_escape("std::mem::drop", CallContext::None);
        assert_eq!(result, None);
    }

    // ── Python tests ────────────────────────────────────────────────────────

    #[cfg(feature = "python")]
    #[test]
    fn test_python_config_produces() {
        let config = ResourceOpConfig::default_for(Language::Python);
        // Exact producers
        assert!(config.is_producer("open"));
        assert!(config.is_producer("sqlite3.connect"));
        assert!(config.is_producer("socket.socket"));
        assert!(config.is_producer("requests.Session"));
        // Suffix "connect" catch-all
        assert!(config.is_producer("db.connect"));
        assert!(config.is_producer("my_connect"));
        // Consumers should NOT be producers
        assert!(!config.is_producer("file.close"));
        assert!(!config.is_producer("os.close"));
        // Unrelated names
        assert!(!config.is_producer("free"));
        assert!(!config.is_producer("dispose"));
    }

    #[cfg(feature = "python")]
    #[test]
    fn test_python_config_consumes() {
        let config = ResourceOpConfig::default_for(Language::Python);
        // .close/.dispose/.release suffixes should match
        assert_eq!(config.is_consumer("file.close"), Some(0));
        assert_eq!(config.is_consumer("conn.dispose"), Some(0));
        assert_eq!(config.is_consumer("resource.release"), Some(0));
        // os.close exact match
        assert_eq!(config.is_consumer("os.close"), Some(0));
        // Producers should NOT be consumers
        assert_eq!(config.is_consumer("open"), None);
        assert_eq!(config.is_consumer("sqlite3.connect"), None);
        // Unrelated
        assert_eq!(config.is_consumer("free"), None);
    }

    #[cfg(feature = "python")]
    #[test]
    fn test_python_config_classify() {
        let config = ResourceOpConfig::default_for(Language::Python);
        // classify_return: producers → NewOwned
        assert_eq!(
            config.classify_return("open"),
            Some(ReturnContract::NewOwned)
        );
        assert_eq!(
            config.classify_return("sqlite3.connect"),
            Some(ReturnContract::NewOwned)
        );
        assert_eq!(config.classify_return("file.close"), None);
        // classify_consumption: consumers with MethodCall style
        let cc = config.classify_consumption("file.close");
        assert!(cc.is_some());
        let contract = cc.unwrap();
        assert_eq!(contract.style, ConsumptionStyle::MethodCall);
        // os.close is ExplicitCall (not method-call style — starts with "os.", no dot before close)
        let cc2 = config.classify_consumption("os.close");
        assert!(cc2.is_some());
        // Producers should not be consumers
        assert!(config.classify_consumption("open").is_none());
    }

    // ── TypeScript / JavaScript tests ───────────────────────────────────────

    #[cfg(feature = "typescript")]
    #[test]
    fn test_ts_config_produces() {
        let config = ResourceOpConfig::default_for(Language::TypeScript);
        // Exact producers
        assert!(config.is_producer("open"));
        assert!(config.is_producer("createReadStream"));
        assert!(config.is_producer("createWriteStream"));
        assert!(config.is_producer("createServer"));
        assert!(config.is_producer("createClient"));
        // React hooks
        assert!(config.is_producer("useEffect"));
        assert!(config.is_producer("useMemo"));
        assert!(config.is_producer("useCallback"));
        // Timer factories
        assert!(config.is_producer("setTimeout"));
        assert!(config.is_producer("setInterval"));
        // Suffix producers
        assert!(config.is_producer("pg.createConnection"));
        assert!(config.is_producer("db.openConnection"));
        // Consumers should NOT be producers
        assert!(!config.is_producer("conn.close"));
        assert!(!config.is_producer("clearTimeout"));
        // Unrelated
        assert!(!config.is_producer("free"));
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_ts_config_consumes() {
        let config = ResourceOpConfig::default_for(Language::TypeScript);
        // Suffix consumers
        assert_eq!(config.is_consumer("conn.dispose"), Some(0));
        assert_eq!(config.is_consumer("file.close"), Some(0));
        assert_eq!(config.is_consumer("obj.destroy"), Some(0));
        assert_eq!(config.is_consumer("res.release"), Some(0));
        // Timer clear functions
        assert_eq!(config.is_consumer("clearTimeout"), Some(0));
        assert_eq!(config.is_consumer("clearInterval"), Some(0));
        // Producers should NOT be consumers
        assert_eq!(config.is_consumer("open"), None);
        assert_eq!(config.is_consumer("createReadStream"), None);
        // Unrelated
        assert_eq!(config.is_consumer("free"), None);
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_ts_config_classify() {
        let config = ResourceOpConfig::default_for(Language::TypeScript);
        // classify_return: normal producers → NewOwned
        assert_eq!(
            config.classify_return("open"),
            Some(ReturnContract::NewOwned)
        );
        assert_eq!(
            config.classify_return("createReadStream"),
            Some(ReturnContract::NewOwned)
        );
        // useEffect → MaybeOwned (subscription resource)
        assert_eq!(
            config.classify_return("useEffect"),
            Some(ReturnContract::MaybeOwned)
        );
        // Other React hooks are regular producers
        assert_eq!(
            config.classify_return("useMemo"),
            Some(ReturnContract::NewOwned)
        );
        assert_eq!(
            config.classify_return("useCallback"),
            Some(ReturnContract::NewOwned)
        );
        // Timer factories are regular producers
        assert_eq!(
            config.classify_return("setTimeout"),
            Some(ReturnContract::NewOwned)
        );
        // Consumers should not be classified as returns
        assert_eq!(config.classify_return("clearTimeout"), None);
        // classify_consumption: consumers with MethodCall style
        let cc = config.classify_consumption("conn.close");
        assert!(cc.is_some());
        assert_eq!(cc.unwrap().style, ConsumptionStyle::MethodCall);
        // clearTimeout is ExplicitCall (no dot)
        let cc2 = config.classify_consumption("clearTimeout");
        assert!(cc2.is_some());
        // Producers should not consume
        assert!(config.classify_consumption("open").is_none());
    }

    // ── Part C: Wildcard matcher test ──────────────────────────────────────

    #[test]
    fn test_wildcard_matcher() {
        let m = CalleeMatcher::Wildcard;
        assert!(m.matches("anything"));
        assert!(m.matches("os.Open"));
        assert!(m.matches(""));
    }
}
