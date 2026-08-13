//! File change detection against the content hashes persisted by the last
//! successful index or sync.

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::discovery::DiscoveryConfig;

/// Compute the BLAKE3 hex hash of a file's **raw** on-disk bytes (file identity).
fn compute_blake3_hex(path: &Path) -> anyhow::Result<String> {
    let content = std::fs::read(path)?;
    Ok(workspace::file_content_hash(&content))
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

/// Detect changes by comparing current file content hashes against DB-stored hashes.
/// Uses `files.content_hash` in the SQLite store as the single source of truth.
pub fn detect_changes(root: &Path, store: &db::Store) -> Result<ChangedFiles> {
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
    let db_files = store.list_files()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

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
    fn clean_git_worktree_is_compared_with_the_indexed_hash() {
        use types::{FileId, FileInfo, Language, ParseStatus};

        let dir = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );

        let path = dir.path().join("main.ts");
        fs::write(&path, "export const version = 2;\n").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "main.ts"])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.name=Atlas Test",
                    "-c",
                    "user.email=atlas@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "current tree",
                ])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(dir.path())
                .output()
                .unwrap()
                .stdout
                .is_empty(),
            "fixture must have a clean worktree"
        );

        let store = db::Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        store
            .upsert_file(&FileInfo {
                file_id: FileId::generate("main.ts"),
                path: "main.ts".into(),
                language: Language::TypeScript,
                content_hash: workspace::file_content_hash(b"export const version = 1;\n"),
                status: ParseStatus::Success,
            })
            .unwrap();

        let changes = detect_changes(dir.path(), &store).unwrap();
        assert_eq!(changes.modified, vec![path]);
    }

    #[test]
    fn test_detect_changes_tempdir() {
        use db::Store;
        use extraction::create_frontend;
        use extraction::{ExtractionMode, extract_file_with_mode};
        use types::Language;
        use types::ids::FileId;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();

        // First detection: empty DB → no changes
        let changes = detect_changes(dir.path(), &store).unwrap();
        assert_eq!(changes.total(), 0, "empty project + empty DB → no changes");

        // Create a new .ts file on disk
        let file_path = dir.path().join("new.ts");
        fs::write(&file_path, b"const x = 1;").unwrap();

        // Should detect as added (on disk but not in DB)
        let changes = detect_changes(dir.path(), &store).unwrap();
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
        let facts = extract_file_with_mode(
            &frontend,
            file_id,
            &relative,
            source,
            &content_hash,
            ExtractionMode::Full,
            &(),
        )
        .unwrap();
        store.insert_file_facts(&facts).unwrap();

        // After indexing: DB hash matches disk hash → no changes
        let changes = detect_changes(dir.path(), &store).unwrap();
        assert_eq!(changes.total(), 0, "indexed file matches → no changes");

        // Modify the file (content hash differs)
        fs::write(&file_path, b"const y = 2;").unwrap();
        let changes = detect_changes(dir.path(), &store).unwrap();
        assert_eq!(changes.modified.len(), 1);
        assert!(changes.modified[0].ends_with("new.ts"));

        // Delete the file (on disk gone, still in DB)
        fs::remove_file(&file_path).unwrap();
        let changes = detect_changes(dir.path(), &store).unwrap();
        assert_eq!(changes.deleted.len(), 1);
        assert!(changes.deleted[0].ends_with("new.ts"));
    }
}
