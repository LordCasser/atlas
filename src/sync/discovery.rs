//! File discovery: git ls-files (preferred) or filesystem walk (fallback).
//!
//! Priorities:
//! 1. `git ls-files --cached --others --exclude-standard` (respects .gitignore)
//! 2. Filesystem walk with hardcoded directory exclusions
//!
//! Both paths filter by language support and optional `.atlasignore` patterns.

use crate::types::Language;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Configuration for file discovery.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryConfig {
    /// Only include files matching these glob patterns (e.g. `["src/**"]`).
    pub include_patterns: Vec<String>,
    /// Exclude files matching these glob patterns.
    pub exclude_patterns: Vec<String>,
}

/// Discover all source files in a project root.
///
/// Returns relative file paths (relative to `root`).
pub fn discover_files(root: &Path, config: &DiscoveryConfig) -> anyhow::Result<Vec<PathBuf>> {
    let files = if is_git_repo(root) {
        discover_via_git(root)?
    } else {
        discover_via_walk(root)?
    };

    // Load .atlasignore patterns
    let atlasignore_patterns = load_atlasignore(root);

    // Filter by language support + .atlasignore + include/exclude config
    let filtered: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| matches_language(p))
        .filter(|p| !matches_any_glob(p, &atlasignore_patterns))
        .filter(|p| !matches_any_glob(p, &config.exclude_patterns))
        .filter(|p| {
            config.include_patterns.is_empty()
                || matches_any_glob(p, &config.include_patterns)
        })
        .collect();

    Ok(filtered)
}

// ── git-based discovery ────────────────────────────────────────────────

fn is_git_repo(root: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn discover_via_git(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let output = match Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "-z"])
        .current_dir(root)
        .output()
    {
        Ok(o) => o,
        Err(_) => return discover_via_walk(root), // git not available — fall back
    };

    if !output.status.success() {
        return discover_via_walk(root); // git failed — fall back
    }

    let files: Vec<PathBuf> = output
        .stdout
        .split(|&b| b == 0)
        .filter_map(|bytes| {
            let s = std::str::from_utf8(bytes).ok()?.trim();
            if s.is_empty() {
                None
            } else {
                Some(PathBuf::from(s))
            }
        })
        .collect();

    Ok(files)
}

// ── filesystem walk (fallback) ─────────────────────────────────────────

const ALWAYS_EXCLUDE_DIRS: &[&str] = &[
    ".git", ".atlas", "node_modules", "target", "__pycache__",
    "venv", ".venv", ".env", "dist", "build", ".next", ".nuxt",
    ".cache", ".mypy_cache", ".pytest_cache", ".tox", ".eggs",
    "bower_components", "vendor",
];

fn discover_via_walk(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    walk_dir(root, root, &mut files)?;
    Ok(files)
}

fn walk_dir(dir: &Path, root: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || ALWAYS_EXCLUDE_DIRS.contains(&name) {
                continue;
            }
            walk_dir(&path, root, files)?;
        } else {
            if let Ok(rel) = path.strip_prefix(root) {
                files.push(rel.to_path_buf());
            }
        }
    }
    Ok(())
}

// ── .atlasignore ──────────────────────────────────────────────────────

fn load_atlasignore(root: &Path) -> Vec<String> {
    let path = root.join(".atlasignore");
    match std::fs::read_to_string(&path) {
        Ok(content) => content
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect(),
        Err(_) => Vec::new(),
    }
}

// ── helpers ───────────────────────────────────────────────────────────

fn matches_language(path: &Path) -> bool {
    Language::from_path(path).is_some()
}

/// Simple glob matching (supports `*` wildcard and `**` for any depth).
fn matches_any_glob(path: &Path, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let path_str = path.to_string_lossy();
    patterns.iter().any(|pat| glob_match(&path_str, pat))
}

fn glob_match(path: &str, pattern: &str) -> bool {
    // No wildcards: exact match or directory prefix
    if !pattern.contains('*') {
        return path == pattern || path.starts_with(&format!("{}/", pattern));
    }

    // "**.ext" — match any-depth file extension
    if pattern.starts_with("**.") {
        let ext = &pattern[2..];
        return path.ends_with(ext);
    }
    // "**" alone — match everything
    if pattern == "**" {
        return true;
    }

    // "**/" inside — match prefix then any filename segment that matches suffix
    if let Some(pos) = pattern.find("**/") {
        let prefix = &pattern[..pos];
        let suffix = &pattern[pos + 3..]; // skip "**/"
        if !prefix.is_empty() && !path.starts_with(prefix) {
            return false;
        }
        let rest = if prefix.is_empty() { path } else { &path[prefix.len()..] };
        // Try each path segment against the suffix glob
        return rest.split('/').any(|segment| glob_match(segment, suffix));
    }

    // "/**" at end — match a directory prefix
    if let Some(pos) = pattern.find("/**") {
        let dir = &pattern[..pos];
        return path.starts_with(dir);
    }

    // Simple * matching — * does NOT cross directory boundaries
    if let Some(pos) = pattern.find('*') {
        let prefix = &pattern[..pos];
        let suffix = &pattern[pos + 1..];

        if !path.starts_with(prefix) {
            return false;
        }
        let rest = &path[prefix.len()..];

        if suffix.is_empty() {
            // Trailing * — matches rest (but not across /)
            return !rest.contains('/');
        }

        if !rest.ends_with(suffix) {
            return false;
        }
        let middle = &rest[..rest.len() - suffix.len()];

        // * portion must not contain /
        return !middle.contains('/');
    }

    false
}

// ── tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_match_exact() {
        assert!(glob_match("foo.ts", "foo.ts"));
        assert!(!glob_match("foo.ts", "bar.ts"));
    }

    #[test]
    fn test_glob_match_wildcard() {
        assert!(glob_match("foo.ts", "*.ts"));
        assert!(!glob_match("foo.js", "*.ts"));
        assert!(glob_match("src/foo.ts", "src/*.ts"));
        assert!(!glob_match("src/bar/foo.ts", "src/*.ts"));
    }

    #[test]
    fn test_glob_match_double_star() {
        // **.ext — any depth
        assert!(glob_match("foo.ts", "**.ts"));
        assert!(glob_match("a/b/c/foo.ts", "**.ts"));
        assert!(!glob_match("foo.js", "**.ts"));

        // **/ — prefix + any depth + suffix
        assert!(glob_match("src/foo.ts", "src/**/*.ts"));
        assert!(glob_match("src/a/b/foo.ts", "src/**/*.ts"));
        assert!(!glob_match("lib/foo.ts", "src/**/*.ts"));
        assert!(!glob_match("src/foo.js", "src/**/*.ts"));
    }

    #[test]
    fn test_glob_match_trailing_star() {
        assert!(glob_match("foobar", "foo*"));
        assert!(glob_match("foo", "foo*"));
        assert!(glob_match("foo.ts", "foo*"));
        assert!(!glob_match("bar", "foo*"));
    }
}
