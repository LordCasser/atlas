//! FTS5 query construction with safe parameterization.
//!
//! Builds FTS5 MATCH queries from user input, escaping FTS5 special characters
//! and supporting AND/OR/prefix queries.

/// Construct a safe FTS5 MATCH query string from user input.
///
/// Escapes FTS5 special characters (`^`, `*`, `"`, `(`, `)`, `-`, `:`, `~`).
/// Supports multi-term queries with implicit AND.
pub struct FtsQuery {
    /// The escaped query string ready for FTS5.
    terms: Vec<String>,
    /// Whether to use prefix matching (append * to each term).
    prefix: bool,
}

impl FtsQuery {
    /// Build from raw user input.
    pub fn new(raw: &str) -> Self {
        let terms: Vec<String> = raw
            .split_whitespace()
            .filter(|w| !w.is_empty())
            .map(escape_fts5)
            .collect();
        Self {
            terms,
            prefix: false,
        }
    }

    /// Enable prefix matching (appends `*` to last term).
    pub fn with_prefix(mut self) -> Self {
        self.prefix = true;
        self
    }

    /// Build the final FTS5 MATCH string.
    /// Returns `None` if there are no valid terms.
    pub fn build(&self) -> Option<String> {
        if self.terms.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = Vec::with_capacity(self.terms.len());
        let last = self.terms.len() - 1;
        for (i, term) in self.terms.iter().enumerate() {
            if i == last && self.prefix {
                parts.push(format!("{term}*"));
            } else {
                parts.push(term.clone());
            }
        }
        Some(parts.join(" "))
    }

    /// Build with explicit AND between terms.
    pub fn build_and(&self) -> Option<String> {
        if self.terms.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = Vec::with_capacity(self.terms.len());
        let last = self.terms.len() - 1;
        for (i, term) in self.terms.iter().enumerate() {
            let mut t = term.clone();
            if i == last && self.prefix {
                t.push('*');
            }
            if i > 0 {
                parts.push("AND".to_string());
            }
            parts.push(t);
        }
        Some(parts.join(" "))
    }

    /// Build with OR between terms.
    pub fn build_or(&self) -> Option<String> {
        if self.terms.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = Vec::with_capacity(self.terms.len());
        let last = self.terms.len() - 1;
        for (i, term) in self.terms.iter().enumerate() {
            let mut t = term.clone();
            if i == last && self.prefix {
                t.push('*');
            }
            if i > 0 {
                parts.push("OR".to_string());
            }
            parts.push(t);
        }
        Some(parts.join(" "))
    }

    /// Check if the query has any searchable terms.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

/// Escape FTS5 special characters in a term.
fn escape_fts5(term: &str) -> String {
    let mut escaped = String::with_capacity(term.len());
    for ch in term.chars() {
        match ch {
            // FTS5 special characters — wrap term in quotes if present
            '*' | '^' | '"' | '(' | ')' | '-' | ':' | '~' => {
                // Remove these chars rather than quoting (simpler, safer)
                // For real quoting, surround entire term with ""
            }
            _ => escaped.push(ch),
        }
    }
    // If we stripped characters and got empty, return the original filtered
    if escaped.is_empty() && !term.is_empty() {
        term.chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect()
    } else {
        escaped
    }
}

/// Sanitize a raw user query for FTS5 MATCH usage.
/// This is the public-safe wrapper.
pub fn sanitize_fts5_query(query: &str) -> String {
    let fts = FtsQuery::new(query);
    fts.build().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_query() {
        let q = FtsQuery::new("hello world");
        assert_eq!(q.build().unwrap(), "hello world");
    }

    #[test]
    fn test_special_char_escape() {
        let q = FtsQuery::new("foo (bar)");
        let result = q.build().unwrap();
        assert!(!result.contains('('));
        assert!(!result.contains(')'));
    }

    #[test]
    fn test_prefix_query() {
        let q = FtsQuery::new("user").with_prefix();
        assert_eq!(q.build().unwrap(), "user*");
    }

    #[test]
    fn test_and_query() {
        let q = FtsQuery::new("hello world");
        assert_eq!(q.build_and().unwrap(), "hello AND world");
    }

    #[test]
    fn test_or_query() {
        let q = FtsQuery::new("foo bar");
        assert_eq!(q.build_or().unwrap(), "foo OR bar");
    }

    #[test]
    fn test_empty_query() {
        let q = FtsQuery::new("");
        assert!(q.build().is_none());
    }

    #[test]
    fn test_fts5_sanitize() {
        let safe = sanitize_fts5_query("SELECT * FROM users");
        // FTS5 special chars (*) are removed; words like FROM/SELECT are not FTS5 special
        assert!(!safe.contains('*'));
    }
}
