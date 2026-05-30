//! Dirty-set computation for full and incremental indexing.
//!
//! The input paths are project-relative paths returned by discovery. The output
//! is also project-relative so callers can decide how to present or process
//! them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use db::Store;
use rayon::prelude::*;
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
    let current_hashes: HashMap<String, String> = discovered
        .par_iter()
        .filter_map(|rel_path| {
            let abs_path = root.join(rel_path);
            let content = std::fs::read(&abs_path).ok()?;
            let hash = blake3::hash(&content).to_hex().to_string();
            let key = SourcePath::try_from_relative(&rel_path.to_string_lossy()).ok()?;
            Some((key.as_str().to_string(), hash))
        })
        .collect();

    let db_files = store.list_files().unwrap_or_default();
    let db_hashes: HashMap<String, String> = db_files
        .iter()
        .map(|f| (f.path.clone(), f.content_hash.clone()))
        .collect();
    let db_paths: HashSet<String> = db_hashes.keys().cloned().collect();

    let mut dirty = Vec::new();
    let mut clean_count = 0usize;
    let discovered_set: HashSet<String> = current_hashes.keys().cloned().collect();

    for rel_path in discovered {
        let key = match SourcePath::try_from_relative(&rel_path.to_string_lossy()) {
            Ok(sp) => sp.as_str().to_string(),
            Err(_) => continue,
        };
        match db_hashes.get(&key) {
            None => dirty.push(rel_path.clone()),
            Some(db_hash) => {
                if let Some(curr_hash) = current_hashes.get(&key) {
                    if curr_hash == db_hash {
                        clean_count += 1;
                    } else {
                        dirty.push(rel_path.clone());
                    }
                } else {
                    dirty.push(rel_path.clone());
                }
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
