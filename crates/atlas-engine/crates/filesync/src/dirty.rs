//! Dirty-set computation for full and incremental indexing.
//!
//! The input paths are project-relative paths returned by discovery. The output
//! is also project-relative so callers can decide how to present or process
//! them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use db::Store;
use extraction::ExtractionMode;
use rayon::prelude::*;
use types::structs::FactCoverage;
use workspace::SourcePath;

/// Files that require re-indexing compared with the store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirtySet {
    pub dirty: Vec<PathBuf>,
    pub clean_count: usize,
    pub deleted: Vec<PathBuf>,
}

/// Compute changed files by comparing current file hashes with stored hashes.
///
/// `discovered` must contain project-relative paths. Paths that cannot be
/// normalized as [`SourcePath`] are ignored, matching extraction behavior.
pub fn build_dirty_set(store: &Store, discovered: &[PathBuf], root: &Path) -> Result<DirtySet> {
    build_dirty_set_with_required_capability(store, discovered, root, FactCoverage::default(), None)
}

/// Compute changed files for a target extraction mode.
///
/// A file is clean only when both its content hash matches and the database
/// already has fresh, complete extraction state for the requested analysis
/// capability. This lets `atlas index --analysis full` upgrade a hash-clean
/// structural DB instead of incorrectly skipping unchanged files.
pub fn build_dirty_set_for_mode(
    store: &Store,
    discovered: &[PathBuf],
    root: &Path,
    mode: &ExtractionMode,
    on_progress: Option<&(dyn Fn(u64) + Sync)>,
) -> Result<DirtySet> {
    build_dirty_set_with_required_capability(
        store,
        discovered,
        root,
        required_capability_for_mode(mode),
        on_progress,
    )
}

fn build_dirty_set_with_required_capability(
    store: &Store,
    discovered: &[PathBuf],
    root: &Path,
    required: FactCoverage,
    on_progress: Option<&(dyn Fn(u64) + Sync)>,
) -> Result<DirtySet> {
    let current_hashes: HashMap<String, String> = discovered
        .par_iter()
        .filter_map(|rel_path| {
            let abs_path = root.join(rel_path);
            let content = std::fs::read(&abs_path).ok()?;
            let hash = workspace::file_content_hash(&content);
            let key = SourcePath::try_from_relative(&rel_path.to_string_lossy()).ok()?;
            Some((key.as_str().to_string(), hash))
        })
        .collect();

    let db_files = store.list_files().unwrap_or_default();
    let db_hashes: HashMap<String, String> = db_files
        .iter()
        .map(|f| (f.path.clone(), f.content_hash.clone()))
        .collect();
    let db_file_by_path: HashMap<String, _> =
        db_files.into_iter().map(|f| (f.path.clone(), f)).collect();
    let db_paths: HashSet<String> = db_hashes.keys().cloned().collect();

    let mut dirty = Vec::new();
    let mut clean_count = 0usize;
    let discovered_set: HashSet<String> = current_hashes.keys().cloned().collect();
    let total = discovered.len();

    for (idx, rel_path) in discovered.iter().enumerate() {
        let key = match SourcePath::try_from_relative(&rel_path.to_string_lossy()) {
            Ok(sp) => sp.as_str().to_string(),
            Err(_) => continue,
        };
        match db_hashes.get(&key) {
            None => dirty.push(rel_path.clone()),
            Some(db_hash) => {
                if let Some(curr_hash) = current_hashes.get(&key) {
                    if curr_hash == db_hash {
                        let has_required = db_file_by_path
                            .get(&key)
                            .map(|file| {
                                store.file_has_fresh_complete_capability(
                                    &file.file_id,
                                    curr_hash,
                                    required,
                                )
                            })
                            .transpose()?
                            .unwrap_or(false);
                        if has_required {
                            clean_count += 1;
                        } else {
                            dirty.push(rel_path.clone());
                        }
                    } else {
                        dirty.push(rel_path.clone());
                    }
                } else {
                    dirty.push(rel_path.clone());
                }
            }
        }
        // Report progress every 50 files or on the last file
        if idx % 50 == 0 || idx + 1 == total {
            if let Some(ref cb) = on_progress {
                cb((idx + 1) as u64);
            }
        }
    }

    let deleted = db_paths
        .difference(&discovered_set)
        .map(PathBuf::from)
        .collect();

    Ok(DirtySet {
        dirty,
        clean_count,
        deleted,
    })
}

fn required_capability_for_mode(mode: &ExtractionMode) -> FactCoverage {
    let mut mask = FactCoverage::default();
    match mode {
        ExtractionMode::Manifest | ExtractionMode::ResolutionSymbols => {
            mask.set(FactCoverage::MANIFEST);
        }
        ExtractionMode::Structural => {
            mask.set(FactCoverage::STRUCTURAL);
        }
        ExtractionMode::Full => {
            mask.set(FactCoverage::DATAFLOW);
        }
        ExtractionMode::LazyDataflow { .. } => {
            mask.set(FactCoverage::DATAFLOW);
        }
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{FileId, FileInfo, Language, ParseStatus};

    #[test]
    fn empty_store_marks_discovered_files_dirty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.ts"), "const value = 1;\n").unwrap();

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();

        let dirty = build_dirty_set(&store, &[PathBuf::from("main.ts")], dir.path()).unwrap();

        assert_eq!(dirty.dirty, vec![PathBuf::from("main.ts")]);
        assert_eq!(dirty.clean_count, 0);
        assert!(dirty.deleted.is_empty());
    }

    #[test]
    fn hash_clean_file_missing_target_capability_is_dirty() {
        let dir = tempfile::tempdir().unwrap();
        let path = PathBuf::from("main.ts");
        let source = "const value = 1;\n";
        std::fs::write(dir.path().join(&path), source).unwrap();
        let hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let file_id = FileId::generate("main.ts");
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "main.ts".into(),
                language: Language::TypeScript,
                content_hash: hash.clone(),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file_extraction_state(
                &file_id,
                "manifest",
                &hash,
                "complete",
                FactCoverage::from_layers(&["manifest"]),
            )
            .unwrap();

        let dirty = build_dirty_set_for_mode(
            &store,
            &[path.clone()],
            dir.path(),
            &ExtractionMode::Structural,
            None,
        )
        .unwrap();

        assert_eq!(dirty.dirty, vec![path]);
        assert_eq!(dirty.clean_count, 0);
    }

    #[test]
    fn dataflow_capability_satisfies_structural_and_full_dirty_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = PathBuf::from("main.ts");
        let source = "const value = 1;\n";
        std::fs::write(dir.path().join(&path), source).unwrap();
        let hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let file_id = FileId::generate("main.ts");
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "main.ts".into(),
                language: Language::TypeScript,
                content_hash: hash.clone(),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file_extraction_state(
                &file_id,
                "dataflow",
                &hash,
                "complete",
                FactCoverage::from_layers(&["dataflow"]),
            )
            .unwrap();

        let structural = build_dirty_set_for_mode(
            &store,
            &[path.clone()],
            dir.path(),
            &ExtractionMode::Structural,
            None,
        )
        .unwrap();
        let full =
            build_dirty_set_for_mode(&store, &[path], dir.path(), &ExtractionMode::Full, None)
                .unwrap();

        assert!(structural.dirty.is_empty());
        assert_eq!(structural.clean_count, 1);
        assert!(full.dirty.is_empty());
        assert_eq!(full.clean_count, 1);
    }
}
