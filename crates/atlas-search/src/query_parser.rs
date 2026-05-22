//! Search query parser: structured field-query syntax.
//!
//! Parses queries like `kind:function lang:typescript path:src name:auth authenticate`
//! into a `ParsedQuery` with typed filters and free-text remainder.
//!
//! ## Syntax
//!
//! - `kind:<SymbolKind>` — filter by symbol kind (e.g. `kind:function`, `kind:class`)
//! - `lang:<Language>` — filter by language (e.g. `lang:typescript`, `lang:python`)
//! - `path:<substring>` — filter by file path substring
//! - `name:<substring>` — filter by symbol name substring
//! - Remaining tokens without a prefix are treated as free-text search terms.
//!
//! ## Examples
//!
//! - `kind:function lang:typescript path:src name:auth authenticate`
//!   → kind_filter=Function, language=TypeScript, path_filter="src",
//!     name_filter="auth", freetext="authenticate"
//!
//! - `lang:python fastapi`
//!   → language=Python, freetext="fastapi"
//!
//! - `useState`
//!   → freetext="useState" (no structured filters)

use atlas_types::{Language, SymbolKind};

// ---------------------------------------------------------------------------
// ParsedQuery
// ---------------------------------------------------------------------------

/// The result of parsing a structured search query.
#[derive(Debug, Clone, Default)]
pub struct ParsedQuery {
    /// Filter by symbol kind (e.g. `kind:function`).
    pub kind_filter: Option<SymbolKind>,
    /// Filter by language (e.g. `lang:typescript`).
    pub language: Option<Language>,
    /// Filter by file path substring (e.g. `path:src`).
    pub path_filter: Option<String>,
    /// Filter by symbol name substring (e.g. `name:auth`).
    pub name_filter: Option<String>,
    /// Free-text search terms (tokens without a prefix).
    pub freetext: String,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a structured search query string into a `ParsedQuery`.
///
/// Unknown field prefixes are treated as free-text.
/// Field values are case-insensitive for `kind` and `lang`.
pub fn parse_query(input: &str) -> ParsedQuery {
    let mut result = ParsedQuery::default();
    let mut freetext_parts: Vec<&str> = Vec::new();

    for token in input.split_whitespace() {
        if let Some((prefix, value)) = token.split_once(':') {
            match prefix.to_lowercase().as_str() {
                "kind" => {
                    result.kind_filter = parse_symbol_kind(value);
                    if result.kind_filter.is_none() {
                        // Unrecognized kind — treat entire token as freetext
                        freetext_parts.push(token);
                    }
                }
                "lang" => {
                    result.language = parse_language(value);
                    if result.language.is_none() {
                        freetext_parts.push(token);
                    }
                }
                "path" => {
                    if !value.is_empty() {
                        result.path_filter = Some(value.to_string());
                    } else {
                        freetext_parts.push(token);
                    }
                }
                "name" => {
                    if !value.is_empty() {
                        result.name_filter = Some(value.to_string());
                    } else {
                        freetext_parts.push(token);
                    }
                }
                _ => {
                    // Unknown prefix — treat as freetext
                    freetext_parts.push(token);
                }
            }
        } else {
            freetext_parts.push(token);
        }
    }

    result.freetext = freetext_parts.join(" ");
    result
}

// ---------------------------------------------------------------------------
// Kind / Language parsers
// ---------------------------------------------------------------------------

/// Parse a case-insensitive symbol kind string.
fn parse_symbol_kind(s: &str) -> Option<SymbolKind> {
    let lower = s.to_lowercase();
    // Map common aliases and canonical names
    match lower.as_str() {
        "function" | "func" | "fn" => Some(SymbolKind::Function),
        "method" => Some(SymbolKind::Method),
        "class" => Some(SymbolKind::Class),
        "struct" => Some(SymbolKind::Struct),
        "interface" | "iface" => Some(SymbolKind::Interface),
        "trait" => Some(SymbolKind::Trait),
        "enum" => Some(SymbolKind::Enum),
        "enummember" | "enum_member" | "variant" => Some(SymbolKind::EnumMember),
        "variable" | "var" => Some(SymbolKind::Variable),
        "constant" | "const" => Some(SymbolKind::Constant),
        "property" | "prop" => Some(SymbolKind::Property),
        "field" => Some(SymbolKind::Field),
        "typealias" | "type_alias" | "type" => Some(SymbolKind::TypeAlias),
        "namespace" | "ns" | "module" => Some(SymbolKind::Module),
        "parameter" | "param" => Some(SymbolKind::Parameter),
        "constructor" | "ctor" => Some(SymbolKind::Constructor),
        "macro" => Some(SymbolKind::Macro),
        "decorator" => Some(SymbolKind::Decorator),
        "package" | "pkg" => Some(SymbolKind::Package),
        _ => None,
    }
}

/// Parse a case-insensitive language string.
fn parse_language(s: &str) -> Option<Language> {
    let lower = s.to_lowercase();
    match lower.as_str() {
        "typescript" | "ts" => Some(Language::TypeScript),
        "javascript" | "js" => Some(Language::JavaScript),
        "python" | "py" => Some(Language::Python),
        "java" => Some(Language::Java),
        "c" => Some(Language::C),
        "cpp" | "c++" => Some(Language::Cpp),
        "arkts" | "ark-ts" | "ets" => Some(Language::ArkTS),
        #[cfg(feature = "cangjie")]
        "cangjie" | "cj" => Some(Language::Cangjie),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty() {
        let q = parse_query("");
        assert!(q.kind_filter.is_none());
        assert!(q.language.is_none());
        assert!(q.path_filter.is_none());
        assert!(q.name_filter.is_none());
        assert!(q.freetext.is_empty());
    }

    #[test]
    fn test_parse_freetext_only() {
        let q = parse_query("authenticate");
        assert_eq!(q.freetext, "authenticate");
        assert!(q.kind_filter.is_none());
    }

    #[test]
    fn test_parse_kind_filter() {
        let q = parse_query("kind:function authenticate");
        assert_eq!(q.kind_filter, Some(SymbolKind::Function));
        assert_eq!(q.freetext, "authenticate");
    }

    #[test]
    fn test_parse_lang_filter() {
        let q = parse_query("lang:typescript useState");
        assert_eq!(q.language, Some(Language::TypeScript));
        assert_eq!(q.freetext, "useState");
    }

    #[test]
    fn test_parse_combined() {
        let q = parse_query("kind:function lang:typescript path:src name:auth authenticate");
        assert_eq!(q.kind_filter, Some(SymbolKind::Function));
        assert_eq!(q.language, Some(Language::TypeScript));
        assert_eq!(q.path_filter.as_deref(), Some("src"));
        assert_eq!(q.name_filter.as_deref(), Some("auth"));
        assert_eq!(q.freetext, "authenticate");
    }

    #[test]
    fn test_parse_kind_aliases() {
        assert_eq!(
            parse_query("kind:fn").kind_filter,
            Some(SymbolKind::Function)
        );
        assert_eq!(
            parse_query("kind:func").kind_filter,
            Some(SymbolKind::Function)
        );
        assert_eq!(
            parse_query("kind:class").kind_filter,
            Some(SymbolKind::Class)
        );
        assert_eq!(
            parse_query("kind:var").kind_filter,
            Some(SymbolKind::Variable)
        );
        assert_eq!(
            parse_query("kind:const").kind_filter,
            Some(SymbolKind::Constant)
        );
        assert_eq!(
            parse_query("kind:prop").kind_filter,
            Some(SymbolKind::Property)
        );
        assert_eq!(
            parse_query("kind:ctor").kind_filter,
            Some(SymbolKind::Constructor)
        );
        assert_eq!(
            parse_query("kind:iface").kind_filter,
            Some(SymbolKind::Interface)
        );
        assert_eq!(
            parse_query("kind:pkg").kind_filter,
            Some(SymbolKind::Package)
        );
    }

    #[test]
    fn test_parse_lang_aliases() {
        assert_eq!(parse_query("lang:ts").language, Some(Language::TypeScript));
        assert_eq!(parse_query("lang:js").language, Some(Language::JavaScript));
        assert_eq!(parse_query("lang:py").language, Some(Language::Python));
        assert_eq!(parse_query("lang:c++").language, Some(Language::Cpp));
        #[cfg(feature = "cangjie")]
        assert_eq!(parse_query("lang:cj").language, Some(Language::Cangjie));
        #[cfg(not(feature = "cangjie"))]
        assert_eq!(parse_query("lang:cj").language, None);
    }

    #[test]
    fn test_parse_unknown_prefix_freetext() {
        let q = parse_query("foo:bar baz");
        assert!(q.kind_filter.is_none());
        assert_eq!(q.freetext, "foo:bar baz");
    }

    #[test]
    fn test_parse_unknown_kind_freetext() {
        let q = parse_query("kind:unknownterm hello");
        // Unrecognized kind value → entire token becomes freetext
        assert!(q.kind_filter.is_none());
        assert_eq!(q.freetext, "kind:unknownterm hello");
    }

    #[test]
    fn test_parse_case_insensitive() {
        let q = parse_query("Kind:Function LANG:TypeScript hello");
        assert_eq!(q.kind_filter, Some(SymbolKind::Function));
        assert_eq!(q.language, Some(Language::TypeScript));
        assert_eq!(q.freetext, "hello");
    }

    #[test]
    fn test_parse_multiple_freetext() {
        let q = parse_query("kind:class user manager");
        assert_eq!(q.kind_filter, Some(SymbolKind::Class));
        assert_eq!(q.freetext, "user manager");
    }

    #[test]
    fn test_parse_empty_field_value() {
        let q = parse_query("kind: path:");
        assert!(q.kind_filter.is_none());
        assert!(q.path_filter.is_none());
        assert_eq!(q.freetext, "kind: path:");
    }
}
