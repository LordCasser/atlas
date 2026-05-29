//! Path alias resolver for TypeScript/JavaScript tsconfig.json and jsconfig.json.
//!
//! Handles `baseUrl` and `paths` mappings, e.g.:
//! ```json
//! {
//!   "compilerOptions": {
//!     "baseUrl": ".",
//!     "paths": {
//!       "@/*": ["src/*"],
//!       "@utils": ["src/utils/index.ts"]
//!     }
//!   }
//! }
//! ```
//!
//! This resolves import paths like `@/components/Button` to
//! `src/components/Button` before the ImportResolver tries to find
//! the target file in the DB.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Resolves TypeScript path aliases from tsconfig.json.
pub struct PathAliasResolver {
    /// Base URL directory (relative to project root).
    pub(crate) base_url: Option<PathBuf>,
    /// Path alias patterns: pattern → list of substitution paths.
    /// Patterns may contain a single `*` wildcard.
    pub(crate) paths: HashMap<String, Vec<String>>,
}

impl PathAliasResolver {
    /// Create a resolver with no aliases (identity resolution).
    pub fn empty() -> Self {
        Self {
            base_url: None,
            paths: HashMap::new(),
        }
    }

    /// Load path aliases from `tsconfig.json` or `jsconfig.json` in a project
    /// root. `tsconfig.json` wins when both are present.
    pub fn from_project_root(root: &Path) -> Self {
        Self::from_tsconfig(&root.join("tsconfig.json"))
            .or_else(|| Self::from_jsconfig(&root.join("jsconfig.json")))
            .unwrap_or_else(Self::empty)
    }

    /// Parse a tsconfig.json file and extract baseUrl + paths.
    ///
    /// Returns `None` if the file doesn't exist or can't be parsed.
    pub fn from_tsconfig(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&content).ok()?;

        let compiler_options = value.get("compilerOptions")?;

        let base_url = compiler_options
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .map(|s| PathBuf::from(s));

        let paths = compiler_options
            .get("paths")
            .and_then(|v| v.as_object())
            .map(|obj| {
                let mut map = HashMap::new();
                for (key, val) in obj {
                    if let Some(arr) = val.as_array() {
                        let substitutions: Vec<String> = arr
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect();
                        if !substitutions.is_empty() {
                            map.insert(key.clone(), substitutions);
                        }
                    }
                }
                map
            })
            .unwrap_or_default();

        Some(Self { base_url, paths })
    }

    /// Parse a jsconfig.json file (same format as tsconfig.json).
    pub fn from_jsconfig(path: &Path) -> Option<Self> {
        // jsconfig.json has the same structure as tsconfig.json
        Self::from_tsconfig(path)
    }

    /// Resolve an import path using the configured path aliases.
    ///
    /// Returns `None` if no alias matches (caller should fall back to
    /// default resolution).
    ///
    /// Resolution algorithm:
    /// 1. Check for exact alias match (no wildcard)
    /// 2. Check for wildcard pattern match (e.g., `@/*` matching `@/foo`)
    /// 3. If baseUrl is set, prepend it to relative paths
    pub fn resolve(&self, import_path: &str) -> Option<String> {
        // Strategy 1: Exact match (no wildcard) — first valid substitution wins.
        if let Some(substitutions) = self.paths.get(import_path) {
            if let Some(sub) = substitutions.first() {
                let resolved = self.apply_base_url(sub);
                return Some(resolved);
            }
        }

        // Strategy 2: Wildcard pattern match
        // Find the longest matching prefix pattern with a wildcard
        let mut best_match: Option<(&str, &str, &Vec<String>)> = None;
        let mut best_len = 0;

        for (pattern, substitutions) in &self.paths {
            if let Some(star_pos) = pattern.find('*') {
                let prefix = &pattern[..star_pos];
                let suffix = &pattern[star_pos + 1..];

                if import_path.starts_with(prefix) && import_path.ends_with(suffix) {
                    let matched_len = prefix.len();
                    if matched_len > best_len {
                        best_len = matched_len;
                        best_match = Some((prefix, suffix, substitutions));
                    }
                }
            }
        }

        if let Some((prefix, suffix, substitutions)) = best_match {
            // Extract the wildcard part
            let wildcard_start = prefix.len();
            let wildcard_end = import_path.len() - suffix.len();
            let wildcard_value = &import_path[wildcard_start..wildcard_end];

            // Apply each substitution, replacing `*` with the wildcard value
            for substitution in substitutions {
                if substitution.contains('*') {
                    let resolved = substitution.replace('*', wildcard_value);
                    return Some(self.apply_base_url(&resolved));
                }
            }

            // If no substitution has a wildcard, return the first one as-is
            if let Some(first) = substitutions.first() {
                return Some(self.apply_base_url(first));
            }
        }

        // Strategy 3: If no alias matches but baseUrl is set (and it's a
        // non-trivial baseUrl, not just "."), and the path is not already
        // relative, prepend baseUrl.
        //
        // When `paths` are configured, unmatched bare specifiers like "lodash"
        // are assumed to be external npm packages — don't resolve them via baseUrl.
        // When only baseUrl is set (no paths), resolve bare specifiers against it.
        if self.paths.is_empty()
            && self.base_url.is_some()
            && !import_path.starts_with('.')
            && !import_path.starts_with('/')
        {
            return Some(self.apply_base_url(import_path));
        }

        None
    }

    /// Apply baseUrl to a resolved path.
    fn apply_base_url(&self, path: &str) -> String {
        if let Some(ref base) = self.base_url {
            let base_str = base.to_string_lossy();
            if base_str.is_empty() || base_str == "." {
                return path.to_string();
            }
            format!(
                "{}/{}",
                base_str.trim_end_matches('/'),
                path.trim_start_matches("./")
            )
        } else {
            path.to_string()
        }
    }

    /// Check if any path aliases are configured.
    pub fn has_aliases(&self) -> bool {
        !self.paths.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_resolver() {
        let resolver = PathAliasResolver::empty();
        assert_eq!(resolver.resolve("@/foo"), None);
        assert!(!resolver.has_aliases());
    }

    #[test]
    fn test_wildcard_pattern() {
        let mut paths = HashMap::new();
        paths.insert("@/*".to_string(), vec!["src/*".to_string()]);

        let resolver = PathAliasResolver {
            base_url: Some(PathBuf::from(".")),
            paths,
        };

        assert_eq!(
            resolver.resolve("@/components/Button"),
            Some("src/components/Button".to_string())
        );
        assert_eq!(resolver.resolve("@/utils"), Some("src/utils".to_string()));
        assert_eq!(resolver.resolve("lodash"), None); // no matching pattern
    }

    #[test]
    fn test_exact_match() {
        let mut paths = HashMap::new();
        paths.insert("@utils".to_string(), vec!["src/utils/index.ts".to_string()]);

        let resolver = PathAliasResolver {
            base_url: None,
            paths,
        };

        assert_eq!(
            resolver.resolve("@utils"),
            Some("src/utils/index.ts".to_string())
        );
        assert_eq!(resolver.resolve("@utils/extra"), None); // doesn't match exact
    }

    #[test]
    fn test_base_url_resolution() {
        let resolver = PathAliasResolver {
            base_url: Some(PathBuf::from("src")),
            paths: HashMap::new(),
        };

        // Non-relative paths should be resolved against baseUrl
        assert_eq!(
            resolver.resolve("utils/helper"),
            Some("src/utils/helper".to_string())
        );
        // Relative paths should not be modified
        assert_eq!(resolver.resolve("./local"), None);
    }

    #[test]
    fn test_longest_prefix_match() {
        let mut paths = HashMap::new();
        paths.insert("@/*".to_string(), vec!["src/*".to_string()]);
        paths.insert(
            "@/utils/*".to_string(),
            vec!["src/shared/utils/*".to_string()],
        );

        let resolver = PathAliasResolver {
            base_url: None,
            paths,
        };

        // More specific pattern should win
        assert_eq!(
            resolver.resolve("@/utils/format"),
            Some("src/shared/utils/format".to_string())
        );
        // Less specific pattern
        assert_eq!(
            resolver.resolve("@/components/Button"),
            Some("src/components/Button".to_string())
        );
    }
}
