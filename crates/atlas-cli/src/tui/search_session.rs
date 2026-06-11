//! TUI search helpers: query parsing and search execution.
//!
//! Provides free functions `parse_query` and `do_search` used by the TUI
//! app and background job workers.  Query parsing follows the same
//! MCP-compatible structured prefix syntax as the engine's `parse_query`.
//!
//! ## Query parsing
//!
//! Queries are parsed for structured prefixes that match MCP search semantics:
//! - `lang:<ext>` — language filter (e.g. `lang:ts`, `lang:py`)
//! - `path:<dir>` — directory scope (e.g. `path:src`)
//! - `name:<term>` — symbol name filter
//! - Legacy `dir/: query` format — directory scope with slash separator
//!
//! Unknown prefixes and the remaining text become the freetext search term.

use atlas_engine::{
    Language, SearchEngine, SearchOptions, SearchResult,
    parse_query as engine_parse_query,
};

/// Parsed TUI search query with separated scope/filter and term.
#[derive(Debug, Clone)]
pub struct ParsedSearch {
    /// The actual search term (without language/scope/name prefixes).
    pub search_term: String,
    /// Language filter extracted from `lang:` prefix or extension alias.
    pub language: Option<Language>,
    /// Scope path filter extracted from `path:` prefix or legacy `dir/:` format.
    pub scope_path: Option<String>,
}

/// Parse a raw TUI search query into a [`ParsedSearch`].
///
/// Handles:
/// - MCP-style structured prefixes: `lang:ts`, `path:src`, `name:auth`
/// - Legacy `dir/: query` format for directory scoping
/// - Plain freetext (substring or symbol name search)
///
/// The resulting [`ParsedSearch`] separates the actual search term from
/// scope/language filters so they can be passed independently to the
/// search engine and lazy structural service.
pub(crate) fn parse_query(raw_query: &str) -> ParsedSearch {
    // Try structured query parsing (lang:ts, path:src style)
    let engineered = engine_parse_query(raw_query);

    let language = engineered.language;
    let structured_scope = engineered.path_filter;

    // Start with freetext or name_filter as the search term
    let mut search_term = engineered.freetext;
    if search_term.is_empty() {
        if let Some(ref name) = engineered.name_filter {
            search_term = name.clone();
        }
    }

    // Legacy "dir/: query" format: check for path-prefix slash syntax
    // (e.g. "src/: handler"). This fires when the raw query does not
    // match structured prefixes (lang:/path:/name:/kind:).
    let mut legacy_scope: Option<String> = None;
    let mut legacy_term: Option<String> = None;
    if let Some((prefix, rest)) = raw_query.split_once('/') {
        if prefix
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
            && !rest.is_empty() && !prefix.contains(' ') {
                legacy_scope = Some(format!("{prefix}/"));
                legacy_term = Some(rest.trim().to_string());
            }
    }

    // Decide scope: structured path_filter wins; legacy is fallback.
    let scope_path = structured_scope.or(legacy_scope);

    // Decide term: when structured freetext is identical to the raw query
    // (meaning no structured prefixes matched at all), prefer the legacy
    // term extracted from the slash split. Otherwise keep the structured
    // freetext (which already has prefixes stripped).
    let final_term = if search_term == raw_query {
        legacy_term.unwrap_or(search_term)
    } else if search_term.is_empty() {
        raw_query.to_string()
    } else {
        search_term
    };

    ParsedSearch {
        search_term: final_term,
        language,
        scope_path,
    }
}

/// Perform a search via the search engine using the parsed search info.
///
/// Builds [`SearchOptions`] from the parsed language/scope_path filters
/// and passes them to the search engine along with the extracted search term.
pub(crate) fn do_search(
    search: &SearchEngine,
    parsed: &ParsedSearch,
    max_results: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    let mut options = SearchOptions::new();
    if let Some(lang) = parsed.language {
        options = options.with_language(lang);
    }
    if let Some(ref path) = parsed.scope_path {
        options = options.with_file_path(path.clone());
    }
    search.search(&parsed.search_term, max_results, &options)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Query parsing tests ─────────────────────────────────────────────

    #[test]
    fn parse_query_freetext() {
        let parsed = parse_query("handler");
        assert_eq!(parsed.search_term, "handler");
        assert!(parsed.language.is_none());
        assert!(parsed.scope_path.is_none());
    }

    #[test]
    fn parse_query_lang_prefix() {
        let parsed = parse_query("lang:ts interface");
        assert_eq!(parsed.search_term, "interface");
        assert_eq!(parsed.language, Some(Language::TypeScript));
        assert!(parsed.scope_path.is_none());
    }

    #[test]
    fn parse_query_lang_cangjie() {
        let parsed = parse_query("lang:cj main");
        #[cfg(feature = "cangjie")]
        {
            assert_eq!(parsed.search_term, "main");
            assert_eq!(parsed.language, Some(Language::Cangjie));
        }
        #[cfg(not(feature = "cangjie"))]
        {
            // When cangjie isn't available, "cj" is not a recognized language.
            // The entire "lang:cj" token becomes freetext.
            assert_eq!(parsed.search_term, "lang:cj main");
            assert!(parsed.language.is_none());
        }
    }

    #[test]
    fn parse_query_path_prefix() {
        let parsed = parse_query("path:src handler");
        assert_eq!(parsed.search_term, "handler");
        assert_eq!(parsed.scope_path.as_deref(), Some("src"));
        assert!(parsed.language.is_none());
    }

    #[test]
    fn parse_query_legacy_slash_scope() {
        let parsed = parse_query("src/ handler");
        assert_eq!(parsed.search_term, "handler");
        assert_eq!(parsed.scope_path.as_deref(), Some("src/"));
        assert!(parsed.language.is_none());
    }

    #[test]
    fn parse_query_combined_prefixes() {
        let parsed = parse_query("lang:ts path:src name:auth authenticate");
        assert_eq!(parsed.search_term, "authenticate");
        assert_eq!(parsed.language, Some(Language::TypeScript));
        assert_eq!(parsed.scope_path.as_deref(), Some("src"));
    }
}
