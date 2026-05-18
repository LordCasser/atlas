//! Built-in/external symbol filtering to avoid meaningless resolution.

pub struct BuiltinFilter;

impl BuiltinFilter {
    /// Check if a symbol name is a known built-in that doesn't exist in the codebase.
    pub fn is_builtin(_name: &str, _lang: crate::types::Language) -> bool {
        // TODO: Phase 7 — implement per-language builtin sets
        false
    }
}
