//! File change detection using git status (primary) or content-hash fallback.

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
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

// ---------------------------------------------------------------------------
// File hash store — persists content hashes in .atlas/file_hashes.json
// for reliable incremental change detection independent of git.
// ---------------------------------------------------------------------------

/// Persistent mapping of relative path → blake3 content hash.
#[derive(Debug, Default)]
pub struct FileHashStore {
    /// relative_path → hex hash string
    hashes: HashMap<String, String>,
}

impl FileHashStore {
    /// Load from `.atlas/file_hashes.json`, returning empty if missing.
    pub fn load(atlas_dir: &Path) -> Result<Self> {
        let path = atlas_dir.join("file_hashes.json");
        match fs::read_to_string(&path) {
            Ok(json) => {
                let map: HashMap<String, String> = serde_json::from_str(&json).unwrap_or_default();
                Ok(Self { hashes: map })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).context("Failed to read file_hashes.json"),
        }
    }

    /// Save to `.atlas/file_hashes.json`.
    pub fn save(&self, atlas_dir: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.hashes)?;
        fs::write(atlas_dir.join("file_hashes.json"), json)?;
        Ok(())
    }

    /// Get the stored hash for a relative path, if any.
    pub fn get(&self, rel_path: &str) -> Option<&str> {
        self.hashes.get(rel_path).map(|s| s.as_str())
    }

    /// Store a hash for a relative path.
    pub fn set(&mut self, rel_path: &str, hash: &str) {
        self.hashes.insert(rel_path.to_string(), hash.to_string());
    }

    /// Remove a tracked path (file deleted).
    pub fn remove(&mut self, rel_path: &str) {
        self.hashes.remove(rel_path);
    }

    /// All tracked relative paths.
    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.hashes.keys()
    }

    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Git-based detection (primary)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Hash-based detection (fallback when git is unavailable)
// ---------------------------------------------------------------------------

/// Detect changes by comparing current file content hashes against stored hashes.
/// Uses `.atlas/file_hashes.json` as the persistent hash store.
/// Returns changes + updates the hash store for the next sync.
pub fn detect_hash_changes(root: &Path, hash_store: &mut FileHashStore) -> Result<ChangedFiles> {
    let mut changes = ChangedFiles::default();
    let mut current_hashes: HashMap<String, String> = HashMap::new();

    let known_extensions: HashSet<&str> = crate::types::Language::all_extensions()
        .iter()
        .map(|s| s.trim_start_matches('.'))
        .collect();

    // Walk the project and compute hashes for all current source files
    collect_and_hash_files(root, root, &known_extensions, &mut current_hashes)?;

    // Detect added and modified
    for (rel_path, hash) in &current_hashes {
        match hash_store.get(rel_path) {
            None => {
                // New file — not in previous hash store
                changes.added.push(root.join(rel_path));
            }
            Some(old_hash) if old_hash != hash => {
                // Content changed
                changes.modified.push(root.join(rel_path));
            }
            _ => {
                // Unchanged — skip
            }
        }
    }

    // Detect deleted files (in hash store but not on disk anymore)
    let prev_paths: Vec<String> = hash_store.paths().cloned().collect();
    for rel_path in &prev_paths {
        if !current_hashes.contains_key(rel_path) {
            changes.deleted.push(root.join(rel_path));
            hash_store.remove(rel_path);
        }
    }

    // Update hash store for next sync
    for (rel_path, hash) in current_hashes {
        hash_store.set(&rel_path, &hash);
    }

    Ok(changes)
}

/// Walk the project tree, collecting relative paths and their blake3 content hashes.
fn collect_and_hash_files(
    project_root: &Path,
    dir: &Path,
    extensions: &HashSet<&str>,
    output: &mut HashMap<String, String>,
) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap_or_default();

        // Skip hidden dirs
        if name.to_str().map_or(true, |n| n.starts_with('.')) {
            continue;
        }

        if path.is_dir() {
            let dir_name = name.to_str().unwrap_or_default();
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
            collect_and_hash_files(project_root, &path, extensions, output)?;
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.contains(ext) {
                    let rel = path
                        .strip_prefix(project_root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();
                    let content = fs::read(&path)?;
                    let hash = blake3::hash(&content).to_hex().to_string();
                    output.insert(rel, hash);
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Legacy: kept for git→mtime transitional compatibility
// ---------------------------------------------------------------------------

/// Legacy fallback using mtime + directory walk (no content hash).
/// Kept for tests referencing the old API.
#[allow(dead_code)]
pub fn detect_mtime_changes(
    root: &Path,
    _indexed_files: &HashSet<PathBuf>,
    _last_indexed: &Path,
) -> Result<ChangedFiles> {
    let mut hash_store = FileHashStore::default();
    detect_hash_changes(root, &mut hash_store)
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

    #[test]
    fn test_file_hash_store_empty() {
        let store = FileHashStore::default();
        assert!(store.is_empty());
        assert_eq!(store.get("foo.ts"), None);
    }

    #[test]
    fn test_file_hash_store_set_get() {
        let mut store = FileHashStore::default();
        store.set("src/main.ts", "abc123");
        assert_eq!(store.get("src/main.ts"), Some("abc123"));
        assert!(!store.is_empty());
        store.remove("src/main.ts");
        assert!(store.is_empty());
    }

    #[test]
    fn test_detect_hash_changes_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let atlas_dir = dir.path().join(".atlas");
        fs::create_dir_all(&atlas_dir).unwrap();

        // First sync: empty → no changes initially, but creates hash store
        let mut hash_store = FileHashStore::default();
        let changes = detect_hash_changes(dir.path(), &mut hash_store).unwrap();
        assert_eq!(changes.total(), 0);

        // Create a new .ts file
        let file_path = dir.path().join("new.ts");
        fs::write(&file_path, b"const x = 1;").unwrap();

        let changes = detect_hash_changes(dir.path(), &mut hash_store).unwrap();
        assert_eq!(changes.added.len(), 1);
        assert!(changes.added[0].ends_with("new.ts"));
        assert_eq!(changes.modified.len(), 0);

        // Save and reload hash store to verify persistence
        hash_store.save(&atlas_dir).unwrap();
        let mut reloaded = FileHashStore::load(&atlas_dir).unwrap();
        assert_eq!(
            reloaded.get("new.ts"),
            Some(hash_store.get("new.ts").unwrap())
        );

        // Modify the file
        fs::write(&file_path, b"const y = 2;").unwrap();
        let changes = detect_hash_changes(dir.path(), &mut reloaded).unwrap();
        assert_eq!(changes.modified.len(), 1);
        assert!(changes.modified[0].ends_with("new.ts"));

        // Delete the file
        fs::remove_file(&file_path).unwrap();
        let changes = detect_hash_changes(dir.path(), &mut reloaded).unwrap();
        assert_eq!(changes.deleted.len(), 1);
        assert!(changes.deleted[0].ends_with("new.ts"));
    }
}
