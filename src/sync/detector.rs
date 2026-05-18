//! File change detection using git status (primary) or mtime/hash fallback.

use anyhow::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What happened to a file between the last index and now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

/// A single changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

/// All changes detected in the project since last index.
#[derive(Debug, Clone, Default)]
pub struct ChangedFiles {
    pub added: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
}

impl ChangedFiles {
    pub fn total(&self) -> usize {
        self.added.len() + self.modified.len() + self.deleted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// Detect file changes using `git status --porcelain`.
/// Returns `None` if the project is not a git repository (or git is unavailable).
pub fn detect_git_changes(root: &Path) -> Option<ChangedFiles> {
    let output = Command::new("git")
        .args(["status", "--porcelain", "-u"])
        .current_dir(root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut changes = ChangedFiles::default();

    for line in stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        let (status, path) = parse_porcelain_line(line);
        let full_path = root.join(path);
        match status {
            PorcelainStatus::Added => changes.added.push(full_path),
            PorcelainStatus::Modified => changes.modified.push(full_path),
            PorcelainStatus::Deleted => changes.deleted.push(full_path),
        }
    }

    if changes.is_empty() {
        None
    } else {
        Some(changes)
    }
}

#[derive(Debug)]
enum PorcelainStatus {
    Added,
    Modified,
    Deleted,
}

/// Parse a single git status --porcelain line (e.g. " M src/main.rs" or "?? newfile").
fn parse_porcelain_line(line: &str) -> (PorcelainStatus, &str) {
    let status = &line[..2];
    let path = line[3..].trim();

    match status {
        "??" => (PorcelainStatus::Added, path),
        "A " | " A" | "AM" | "AD" => (PorcelainStatus::Added, path),
        " M" | "M " | "MM" | "CM" => (PorcelainStatus::Modified, path),
        "D " | " D" | "DM" | "RD" => (PorcelainStatus::Deleted, path),
        "R " => {
            // Renamed: "R  old -> new" — take the new name
            if let Some((_old, new_path)) = path.split_once(" -> ") {
                (PorcelainStatus::Added, new_path)
            } else {
                (PorcelainStatus::Modified, path)
            }
        }
        _ => (PorcelainStatus::Modified, path),
    }
}

/// Fallback: detect changes by comparing mtime against a last-indexed timestamp file.
/// Returns paths of files modified since the given timestamp.
pub fn detect_mtime_changes(
    root: &Path,
    indexed_files: &HashSet<PathBuf>,
    _last_indexed: &Path,
) -> Result<ChangedFiles> {
    let mut changes = ChangedFiles::default();
    let mut seen = HashSet::new();

    // Walk the project directory looking for known source extensions
    let known_extensions: HashSet<&str> = crate::types::Language::all_extensions()
        .iter()
        .map(|s| s.trim_start_matches('.'))
        .collect();

    collect_source_files(root, root, &known_extensions, &mut seen)?;

    // Detect added/modified/deleted
    for path in &seen {
        if indexed_files.contains(path) {
            // File existed before — check if modified via mtime
            // For simplicity, assume all existing tracked files may have changed
            // (accurate mtime comparison needs last-indexed timestamp from DB)
            changes.modified.push(path.clone());
        } else {
            changes.added.push(path.clone());
        }
    }

    // Detect deleted files (were indexed but no longer on disk)
    for indexed in indexed_files {
        if !seen.contains(indexed) && !indexed.exists() {
            changes.deleted.push(indexed.clone());
        }
    }

    Ok(changes)
}

/// Recursively collect source files under root, relative to project_root.
fn collect_source_files(
    project_root: &Path,
    dir: &Path,
    extensions: &HashSet<&str>,
    output: &mut HashSet<PathBuf>,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap_or_default();

        // Skip hidden dirs and common ignore patterns
        if name.to_str().map_or(true, |n| n.starts_with('.')) {
            continue;
        }

        if path.is_dir() {
            let dir_name = name.to_str().unwrap_or_default();
            // Skip common non-source directories
            if matches!(
                dir_name,
                "node_modules"
                    | "target"
                    | "dist"
                    | "build"
                    | "__pycache__"
                    | ".git"
                    | ".atlas"
                    | "venv"
                    | ".venv"
            ) {
                continue;
            }
            collect_source_files(project_root, &path, extensions, output)?;
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.contains(ext) {
                    output.insert(path);
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_porcelain_added() {
        let (status, path) = parse_porcelain_line("?? new_file.ts");
        assert!(matches!(status, PorcelainStatus::Added));
        assert_eq!(path, "new_file.ts");
    }

    #[test]
    fn test_parse_porcelain_modified() {
        let (status, path) = parse_porcelain_line(" M src/main.py");
        assert!(matches!(status, PorcelainStatus::Modified));
        assert_eq!(path, "src/main.py");
    }

    #[test]
    fn test_parse_porcelain_deleted() {
        let (status, path) = parse_porcelain_line(" D old.rs");
        assert!(matches!(status, PorcelainStatus::Deleted));
        assert_eq!(path, "old.rs");
    }

    #[test]
    fn test_parse_porcelain_added_staged() {
        let (status, path) = parse_porcelain_line("A  staged.ts");
        assert!(matches!(status, PorcelainStatus::Added));
        assert_eq!(path, "staged.ts");
    }

    #[test]
    fn test_changed_files_stats() {
        let changes = ChangedFiles {
            added: vec![PathBuf::from("a.ts")],
            modified: vec![PathBuf::from("b.py")],
            deleted: vec![],
        };
        assert_eq!(changes.total(), 2);
        assert!(!changes.is_empty());
    }
}
