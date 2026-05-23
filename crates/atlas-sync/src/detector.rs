//! File change detection using git status (primary) or DB content-hash fallback.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::discovery::DiscoveryConfig;

/// Compute the BLAKE3 hex hash of a file's contents.
fn compute_blake3_hex(path: &Path) -> anyhow::Result<String> {
    let content = std::fs::read(path)?;
    Ok(blake3::hash(&content).to_hex().to_string())
}

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
    /// Whether `tsconfig.json` has changed since the last sync.
    /// When true, the sync engine invalidates all import resolutions and
    /// rebuilds edges, because path aliases may have changed.
    ///
    /// Note: only `tsconfig.json` is currently supported; `jsconfig.json`
    /// is not checked (the resolver loads tsconfig only).
    pub tsconfig_changed: bool,
}

impl ChangedFiles {
    pub fn total(&self) -> usize {
        self.added.len() + self.modified.len() + self.deleted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0 && !self.tsconfig_changed
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
            PorcelainStatus::Renamed(new_path) => {
                // Old file data must be cleaned, new file must be indexed
                changes.deleted.push(full_path);
                changes.added.push(root.join(new_path));
            }
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
    Renamed(String), // new path after rename
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
            if let Some((old_path, new_path)) = path.split_once(" -> ") {
                (PorcelainStatus::Renamed(new_path.trim().to_string()), old_path.trim())
            } else {
                (PorcelainStatus::Modified, path)
            }
        }
        _ => (PorcelainStatus::Modified, path),
    }
}

// ---------------------------------------------------------------------------
// DB hash-based detection (fallback when git is unavailable)
// ---------------------------------------------------------------------------

/// Detect changes by comparing current file content hashes against DB-stored hashes.
/// Uses `files.content_hash` in the SQLite store as the single source of truth.
pub fn detect_db_hash_changes(root: &Path, store: &atlas_db::Store) -> Result<ChangedFiles> {
    let mut changes = ChangedFiles::default();

    // 1. Use the same discovery logic as normal index (atlasignore, gitignore,
    //    exclude dirs) to get the canonical file list.
    let config = DiscoveryConfig::default();
    let discovered = crate::discovery::discover_files(root, &config)?;

    let mut current_hashes: HashMap<String, String> = HashMap::new();
    for rel_path in &discovered {
        let full = root.join(rel_path);
        let hash = compute_blake3_hex(&full)?;
        current_hashes.insert(rel_path.to_string_lossy().to_string(), hash);
    }

    // 2. Get previously indexed file hashes from the DB
    let db_files = store.list_files().unwrap_or_default();
    let db_hashes: HashMap<String, String> = db_files
        .iter()
        .map(|f| (f.path.clone(), f.content_hash.clone()))
        .collect();

    // 3. Detect added and modified files (on disk, possibly different from DB)
    for (rel_path, hash) in &current_hashes {
        match db_hashes.get(rel_path) {
            None => {
                changes.added.push(root.join(rel_path));
            }
            Some(old_hash) if old_hash != hash => {
                changes.modified.push(root.join(rel_path));
            }
            _ => { /* unchanged */ }
        }
    }

    // 4. Detect deleted files (in DB but no longer on disk)
    for f in &db_files {
        if !current_hashes.contains_key(&f.path) {
            changes.deleted.push(root.join(&f.path));
        }
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
            tsconfig_changed: false,
        };
        assert_eq!(changes.total(), 2);
        assert!(!changes.is_empty());
    }

    #[test]
    fn test_detect_db_hash_changes_tempdir() {
        use atlas_db::Store;
        use atlas_extraction::create_frontend;
        use atlas_extraction::extract_file;
        use atlas_types::Language;
        use atlas_types::ids::FileId;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();

        // First detection: empty DB → no changes
        let changes = detect_db_hash_changes(dir.path(), &store).unwrap();
        assert_eq!(changes.total(), 0, "empty project + empty DB → no changes");

        // Create a new .ts file on disk
        let file_path = dir.path().join("new.ts");
        fs::write(&file_path, b"const x = 1;").unwrap();

        // Should detect as added (on disk but not in DB)
        let changes = detect_db_hash_changes(dir.path(), &store).unwrap();
        assert_eq!(changes.added.len(), 1);
        assert!(changes.added[0].ends_with("new.ts"));
        assert_eq!(changes.modified.len(), 0);

        // Index the file into the DB
        let relative = PathBuf::from("new.ts");
        let file_id = FileId::generate(&relative.to_string_lossy());
        let lang = Language::TypeScript;
        let frontend = create_frontend(lang).unwrap();
        let source = "const x = 1;";
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let facts = extract_file(&frontend, file_id, &relative, source, &content_hash).unwrap();
        store.insert_file_facts(&facts).unwrap();

        // After indexing: DB hash matches disk hash → no changes
        let changes = detect_db_hash_changes(dir.path(), &store).unwrap();
        assert_eq!(changes.total(), 0, "indexed file matches → no changes");

        // Modify the file (content hash differs)
        fs::write(&file_path, b"const y = 2;").unwrap();
        let changes = detect_db_hash_changes(dir.path(), &store).unwrap();
        assert_eq!(changes.modified.len(), 1);
        assert!(changes.modified[0].ends_with("new.ts"));

        // Delete the file (on disk gone, still in DB)
        fs::remove_file(&file_path).unwrap();
        let changes = detect_db_hash_changes(dir.path(), &store).unwrap();
        assert_eq!(changes.deleted.len(), 1);
        assert!(changes.deleted[0].ends_with("new.ts"));
    }
}
