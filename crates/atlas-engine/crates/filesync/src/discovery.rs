//! File discovery: git ls-files (preferred) or filesystem walk (fallback).
//!
//! Priorities:
//! 1. `git ls-files --cached --others --exclude-standard` (respects .gitignore)
//! 2. Filesystem walk with hardcoded directory exclusions
//!
//! Both paths filter by language support and optional `.atlasignore` patterns.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use types::Language;

/// Configuration for file discovery.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryConfig {
    /// Only include files matching these glob patterns (e.g. `["src/**"]`).
    pub include_patterns: Vec<String>,
    /// Exclude files matching these glob patterns.
    pub exclude_patterns: Vec<String>,
}

fn is_supported_source_path(path: &Path) -> bool {
    Language::from_path(path).is_some()
}

/// Discover all source files in a project root.
///
/// Returns relative file paths (relative to `root`).
pub fn discover_files(root: &Path, config: &DiscoveryConfig) -> anyhow::Result<Vec<PathBuf>> {
    let raw_files = if is_git_repo(root) {
        let git_files = discover_via_git(root)?;
        // When git ls-files returns nothing (e.g. all files gitignored),
        // fall back to filesystem walk so non-tracked directories work.
        if git_files.is_empty() {
            discover_via_walk(root)?
        } else {
            git_files
        }
    } else {
        discover_via_walk(root)?
    };

    let atlasignore_patterns = load_atlasignore(root);

    let filtered: Vec<PathBuf> = raw_files
        .into_iter()
        .filter(|p| should_include(p, config, &atlasignore_patterns))
        .collect();

    Ok(filtered)
}

/// Discover source files without allowing a request path to scan indefinitely.
///
/// The boolean is `true` only when discovery reached the end of the selected
/// scope. A `false` result is a usable partial snapshot, never proof that the
/// returned paths are the complete project inventory.
pub fn discover_files_bounded(
    root: &Path,
    config: &DiscoveryConfig,
    max_files: usize,
    timeout: Duration,
) -> anyhow::Result<(Vec<PathBuf>, bool)> {
    let deadline = Instant::now() + timeout;
    let atlasignore_patterns = load_atlasignore(root);

    if let Some((files, complete, raw_count)) =
        discover_via_git_bounded(root, config, &atlasignore_patterns, max_files, deadline)?
    {
        if raw_count > 0 || !complete {
            return Ok((files, complete));
        }
    }

    discover_via_walk_bounded(root, config, &atlasignore_patterns, max_files, deadline)
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
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
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

fn discover_via_git_bounded(
    root: &Path,
    config: &DiscoveryConfig,
    atlasignore_patterns: &[String],
    max_files: usize,
    deadline: Instant,
) -> anyhow::Result<Option<(Vec<PathBuf>, bool, usize)>> {
    let mut child = match Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return Ok(None),
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(None);
    };
    let (sender, receiver) = mpsc::sync_channel(256);
    let reader = std::thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match stdout.read_until(0, &mut bytes) {
                Ok(0) => break,
                Ok(_) => {
                    if bytes.last() == Some(&0) {
                        bytes.pop();
                    }
                    let Ok(path) = std::str::from_utf8(&bytes) else {
                        continue;
                    };
                    if sender.send(PathBuf::from(path)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut files = Vec::new();
    let mut raw_count = 0usize;
    loop {
        if files.len() >= max_files || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            drop(receiver);
            let _ = reader.join();
            return Ok(Some((files, false, raw_count)));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(path) => {
                raw_count += 1;
                if should_include(&path, config, atlasignore_patterns) {
                    files.push(path);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(receiver);
                let _ = reader.join();
                return Ok(Some((files, false, raw_count)));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let status = child.wait()?;
                let _ = reader.join();
                return if status.success() {
                    Ok(Some((files, true, raw_count)))
                } else {
                    Ok(None)
                };
            }
        }
    }
}

// ── filesystem walk (fallback) ─────────────────────────────────────────

const ALWAYS_EXCLUDE_DIRS: &[&str] = &[
    ".git",
    ".atlas",
    "node_modules",
    "target",
    "__pycache__",
    "venv",
    ".venv",
    ".env",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".cache",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    ".eggs",
    "bower_components",
    "vendor",
];

fn discover_via_walk(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    walk_dir(root, root, &mut files)?;
    Ok(files)
}

fn discover_via_walk_bounded(
    root: &Path,
    config: &DiscoveryConfig,
    atlasignore_patterns: &[String],
    max_files: usize,
    deadline: Instant,
) -> anyhow::Result<(Vec<PathBuf>, bool)> {
    let mut files = Vec::new();
    let root_canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let complete = walk_dir_bounded(
        root,
        root,
        &root_canonical,
        config,
        atlasignore_patterns,
        max_files,
        deadline,
        &mut files,
    )?;
    Ok((files, complete))
}

#[allow(clippy::too_many_arguments)]
fn walk_dir_bounded(
    dir: &Path,
    root: &Path,
    root_canonical: &Path,
    config: &DiscoveryConfig,
    atlasignore_patterns: &[String],
    max_files: usize,
    deadline: Instant,
    files: &mut Vec<PathBuf>,
) -> anyhow::Result<bool> {
    if files.len() >= max_files || Instant::now() >= deadline {
        return Ok(false);
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(true),
    };
    for entry in entries.flatten() {
        if files.len() >= max_files || Instant::now() >= deadline {
            return Ok(false);
        }
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.is_dir() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if name.starts_with('.') || ALWAYS_EXCLUDE_DIRS.contains(&name) {
                continue;
            }
            let canonical = match path.canonicalize() {
                Ok(path) => path,
                Err(_) => continue,
            };
            if !canonical.starts_with(root_canonical)
                || !walk_dir_bounded(
                    &path,
                    root,
                    root_canonical,
                    config,
                    atlasignore_patterns,
                    max_files,
                    deadline,
                    files,
                )?
            {
                return Ok(false);
            }
        } else if meta.is_file()
            && let Ok(relative) = path.strip_prefix(root)
            && should_include(relative, config, atlasignore_patterns)
        {
            files.push(relative.to_path_buf());
        }
    }
    Ok(true)
}

fn walk_dir(dir: &Path, root: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        // Use symlink_metadata to avoid following symlinks into loops
        // or escaping the project root.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || ALWAYS_EXCLUDE_DIRS.contains(&name) {
                continue;
            }
            // Canonicalize to detect symlink escapes
            let canonical = match path.canonicalize() {
                Ok(c) => c,
                Err(_) => continue, // broken symlink — skip
            };
            let root_canonical = match root.canonicalize() {
                Ok(r) => r,
                Err(_) => continue,
            };
            if !canonical.starts_with(&root_canonical) {
                // Symlink points outside project root — skip
                continue;
            }
            walk_dir(&path, root, files)?;
        } else if meta.is_file() {
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

/// Simple glob matching (supports `*` wildcard and `**` for any depth).
fn matches_any_glob(path: &Path, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let path_str = path.to_string_lossy();
    patterns.iter().any(|pat| glob_match(&path_str, pat))
}

fn should_include(path: &Path, config: &DiscoveryConfig, atlasignore: &[String]) -> bool {
    is_supported_source_path(path)
        && !matches_any_glob(path, atlasignore)
        && !matches_any_glob(path, &config.exclude_patterns)
        && (config.include_patterns.is_empty() || matches_any_glob(path, &config.include_patterns))
}

fn glob_match(path: &str, pattern: &str) -> bool {
    // No wildcards: exact match or directory prefix
    if !pattern.contains('*') {
        return path == pattern || path.starts_with(&format!("{pattern}/"));
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
        let rest = if prefix.is_empty() {
            path
        } else {
            &path[prefix.len()..]
        };
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

    #[test]
    fn bounded_discovery_reports_limit_as_partial() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        for index in 0..4 {
            std::fs::write(
                root.path().join(format!("src/file_{index}.ts")),
                "export const value = 1;\n",
            )
            .unwrap();
        }

        let (files, complete) = discover_files_bounded(
            root.path(),
            &DiscoveryConfig {
                include_patterns: vec!["src/**".into()],
                exclude_patterns: Vec::new(),
            },
            2,
            Duration::from_secs(1),
        )
        .unwrap();

        assert_eq!(files.len(), 2);
        assert!(!complete);
        assert!(files.iter().all(|path| path.starts_with("src")));
    }
}
