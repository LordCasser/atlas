//! File change detection against the content hashes persisted by the last
//! successful index or sync.

use anyhow::{Context, Result};
use extraction::ExtractionMode;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use workspace::SourcePath;

use crate::dirty::build_dirty_set_for_mode;
use crate::discovery::DiscoveryConfig;

/// Compute the BLAKE3 hex hash of a file's **raw** on-disk bytes (file identity).
fn compute_blake3_hex(path: &Path) -> anyhow::Result<String> {
    let content = std::fs::read(path)?;
    Ok(workspace::file_content_hash(&content))
}

/// Load the source-discovery scope persisted by the last successful index.
///
/// Databases created before `indexed_scope` existed are treated as legacy
/// whole-project indexes. A present but malformed value is an authority error:
/// silently widening it to the whole project would violate the recorded scope.
fn sync_discovery_config(store: &db::Store) -> Result<DiscoveryConfig> {
    let Some(raw_scope) = store
        .get_metadata("indexed_scope")
        .context("Failed to read indexed_scope metadata")?
    else {
        tracing::warn!(
            target: "atlas_sync",
            "indexed_scope metadata is missing; treating the database as a legacy whole-project index"
        );
        return Ok(DiscoveryConfig::default());
    };

    let scope: serde_json::Value = serde_json::from_str(&raw_scope)
        .context("indexed_scope metadata is not valid JSON; re-run atlas index")?;
    let object = scope.as_object().context(
        "indexed_scope metadata must be an object with include/exclude arrays; re-run atlas index",
    )?;

    let parse_patterns = |field: &str| -> Result<Vec<String>> {
        object
            .get(field)
            .and_then(serde_json::Value::as_array)
            .with_context(|| {
                format!("indexed_scope.{field} must be an array of strings; re-run atlas index")
            })?
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value.as_str().map(str::to_owned).with_context(|| {
                    format!("indexed_scope.{field}[{index}] must be a string; re-run atlas index")
                })
            })
            .collect()
    };

    Ok(DiscoveryConfig {
        include_patterns: parse_patterns("include")?,
        exclude_patterns: parse_patterns("exclude")?,
    })
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

    // 1. Use the same discovery logic and persisted scope as the last index
    //    (atlasignore, gitignore, include/exclude, and default excluded dirs).
    let config = sync_discovery_config(store)?;
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

/// Detect files that must be synchronized for a target extraction mode.
///
/// Unlike [`detect_changes`], this treats a hash-clean indexed file as modified
/// when its fresh, complete extraction state does not satisfy `mode`. The
/// capability decision remains centralized in [`build_dirty_set_for_mode`].
pub fn detect_changes_for_mode(
    root: &Path,
    store: &db::Store,
    mode: &ExtractionMode,
) -> Result<ChangedFiles> {
    let config = sync_discovery_config(store)?;
    let discovered = crate::discovery::discover_files(root, &config)?;
    let dirty_set = build_dirty_set_for_mode(store, &discovered, root, mode, None)?;
    let indexed_paths: HashSet<String> = store
        .list_files()?
        .into_iter()
        .map(|file| file.path)
        .collect();

    let mut changes = ChangedFiles {
        deleted: dirty_set
            .deleted
            .into_iter()
            .map(|path| root.join(path))
            .collect(),
        ..Default::default()
    };

    for rel_path in dirty_set.dirty {
        let key = SourcePath::try_from_relative(&rel_path.to_string_lossy())?;
        let absolute = root.join(&rel_path);
        if indexed_paths.contains(key.as_str()) {
            changes.modified.push(absolute);
        } else {
            changes.added.push(absolute);
        }
    }

    changes.added.sort();
    changes.modified.sort();
    changes.deleted.sort();
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

    fn set_scope(store: &db::Store, include: &[&str], exclude: &[&str]) {
        store
            .set_metadata(
                "indexed_scope",
                &serde_json::json!({
                    "include": include,
                    "exclude": exclude,
                })
                .to_string(),
            )
            .unwrap();
    }

    #[test]
    fn sync_scope_loader_accepts_current_shape_and_legacy_missing_key() {
        let store = db::Store::open_in_memory().unwrap();
        store.init_schema().unwrap();

        assert_eq!(
            sync_discovery_config(&store).unwrap().include_patterns,
            Vec::<String>::new()
        );
        store
            .set_metadata(
                "indexed_scope",
                r#"{"include":["src/**"],"exclude":["src/generated/**"],"future":true}"#,
            )
            .unwrap();

        let config = sync_discovery_config(&store).unwrap();
        assert_eq!(config.include_patterns, ["src/**"]);
        assert_eq!(config.exclude_patterns, ["src/generated/**"]);

        set_scope(&store, &[], &[]);
        let whole_project = sync_discovery_config(&store).unwrap();
        assert!(whole_project.include_patterns.is_empty());
        assert!(whole_project.exclude_patterns.is_empty());
    }

    #[test]
    fn sync_scope_loader_rejects_malformed_authority() {
        let store = db::Store::open_in_memory().unwrap();
        store.init_schema().unwrap();

        for malformed in [
            "not-json",
            "[]",
            r#"{"include":["src/**"]}"#,
            r#"{"include":"src/**","exclude":[]}"#,
            r#"{"include":[1],"exclude":[]}"#,
        ] {
            store.set_metadata("indexed_scope", malformed).unwrap();
            let error = sync_discovery_config(&store)
                .expect_err("malformed indexed_scope must not widen discovery");
            assert!(error.to_string().contains("indexed_scope"), "{error:#}");
        }
    }

    #[test]
    fn scoped_detection_filters_additions_and_reconciles_out_of_scope_cache() {
        use types::{FileId, FileInfo, Language, ParseStatus};

        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/generated")).unwrap();
        fs::create_dir_all(dir.path().join("other")).unwrap();
        let src = dir.path().join("src/main.ts");
        let src_new = dir.path().join("src/new.ts");
        let generated = dir.path().join("src/generated/code.ts");
        let outside = dir.path().join("other/dep.ts");
        fs::write(&src, "export const main = 1;\n").unwrap();
        fs::write(&src_new, "export const fresh = 2;\n").unwrap();
        fs::write(&generated, "export const generated = 2;\n").unwrap();
        fs::write(&outside, "export const outside = 3;\n").unwrap();

        let store = db::Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        set_scope(&store, &["src/**"], &["src/generated/**"]);
        store
            .upsert_file(&FileInfo {
                file_id: FileId::generate("src/main.ts"),
                path: "src/main.ts".into(),
                language: Language::TypeScript,
                content_hash: workspace::file_content_hash(b"export const main = 0;\n"),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file(&FileInfo {
                file_id: FileId::generate("other/dep.ts"),
                path: "other/dep.ts".into(),
                language: Language::TypeScript,
                content_hash: workspace::file_content_hash(b"export const outside = 3;\n"),
                status: ParseStatus::Success,
            })
            .unwrap();

        let changes = detect_changes(dir.path(), &store).unwrap();

        assert_eq!(changes.added, vec![src_new]);
        assert_eq!(changes.modified, vec![src]);
        assert_eq!(changes.deleted, vec![outside]);
        assert!(!changes.added.contains(&generated));
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
    fn mode_aware_detection_reindexes_hash_clean_files_missing_capability() {
        use types::{FactCoverage, FileId, FileInfo, Language, ParseStatus};

        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join("other")).unwrap();
        let path = dir.path().join("src/main.ts");
        let outside = dir.path().join("other/dep.ts");
        let source = b"export const value = 1;\n";
        fs::write(&path, source).unwrap();
        fs::write(&outside, "export const outside = 2;\n").unwrap();
        let content_hash = workspace::file_content_hash(source);

        let store = db::Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        set_scope(&store, &["src/**"], &[]);
        let file_id = FileId::generate("src/main.ts");
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "src/main.ts".into(),
                language: Language::TypeScript,
                content_hash: content_hash.clone(),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file_extraction_state(
                &file_id,
                "manifest",
                &content_hash,
                "complete",
                FactCoverage::from_layers(&["manifest"]),
            )
            .unwrap();

        let changes =
            detect_changes_for_mode(dir.path(), &store, &ExtractionMode::Structural).unwrap();

        assert!(changes.added.is_empty());
        assert_eq!(changes.modified, vec![path]);
        assert!(changes.deleted.is_empty());
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
