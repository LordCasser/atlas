//! Content-hash-based change detection for project configuration files.
//!
//! Detects whether watched config files (e.g. `tsconfig.json`) have appeared,
//! disappeared, or changed since the last run by comparing blake3 content hashes
//! stored in `project_metadata`. Used by the CLI `index` command and the
//! incremental `sync` pipeline to decide when to invalidate resolved references
//! and rebuild structural edges.
//!
//! # Two-phase API (P1: avoid double-detection side effects)
//!
//! [`detect_config_change`] is **read-only** — it checks whether any config
//! changed but does NOT mutate `project_metadata`.  After the caller performs
//! invalidation (if needed), [`commit_config_hashes`] records the current
//! hashes as the new baseline.
//!
//! This split prevents "double detect" scenarios (e.g. `sync` CLI preview +
//! `sync()` internals) where the first detection writes metadata and the
//! second detection sees "no change", silently skipping invalidation.

use std::path::Path;

use anyhow::{Context, Result};
use db::Store;

/// Check whether any watched config file changed since the last commit.
///
/// **Read-only.**  Does NOT write to `project_metadata`.  After the caller
/// completes any necessary invalidation, call [`commit_config_hashes`] to
/// record the new baseline.
///
/// For each `name` in `names`:
/// 1. Reads the file from `root/<name>` and computes its blake3 content hash.
/// 2. Compares the hash with the value stored under key `<name>_hash` in
///    `project_metadata`.
///
/// Returns `true` if any config file appeared, disappeared, or changed.
///
/// # Errors
///
/// Propagates database errors and file I/O errors (except `NotFound`, which
/// is treated as "no current file").
pub fn detect_config_change(store: &Store, root: &Path, names: &[&str]) -> Result<bool> {
    for name in names {
        let config_path = root.join(name);
        let meta_key = format!("{}_hash", name);

        let prev_hash = store
            .get_metadata(&meta_key)
            .with_context(|| format!("failed to read metadata key {meta_key}"))?;

        let current_hash = match std::fs::read(&config_path) {
            Ok(bytes) => Some(blake3::hash(&bytes).to_hex().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(e).with_context(|| format!("failed to read {}", config_path.display()));
            }
        };

        match (&prev_hash, &current_hash) {
            (Some(prev), Some(curr)) if prev == curr => continue,
            (None, None) => continue,
            _ => return Ok(true),
        }
    }
    Ok(false)
}

/// Record current config file hashes as the new comparison baseline.
///
/// Call this **after** a change was detected and the corresponding
/// invalidation completed successfully.  For each `name` in `names`:
/// - If the file exists, its blake3 hash is stored under `<name>_hash`.
/// - If the file is missing, the `<name>_hash` key is deleted so
///   subsequent runs don't repeatedly report a spurious change.
///
/// # Errors
///
/// Propagates database errors and file I/O errors (except `NotFound`,
/// which triggers a metadata delete).
pub fn commit_config_hashes(store: &Store, root: &Path, names: &[&str]) -> Result<()> {
    for name in names {
        let config_path = root.join(name);
        let meta_key = format!("{}_hash", name);

        match std::fs::read(&config_path) {
            Ok(bytes) => {
                let hash = blake3::hash(&bytes).to_hex().to_string();
                store
                    .set_metadata(&meta_key, &hash)
                    .with_context(|| format!("failed to write metadata key {meta_key}"))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                store
                    .delete_metadata(&meta_key)
                    .with_context(|| format!("failed to delete metadata key {meta_key}"))?;
            }
            Err(e) => {
                return Err(e).with_context(|| format!("failed to read {}", config_path.display()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (Store, TempDir) {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let tmp = TempDir::new().unwrap();
        (store, tmp)
    }

    #[test]
    fn no_config_no_previous_run_no_change() {
        let (store, tmp) = setup();
        let changed = detect_config_change(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        assert!(!changed, "missing file with no stored hash is not a change");
    }

    #[test]
    fn config_appears_is_change() {
        let (store, tmp) = setup();
        fs::write(
            tmp.path().join("tsconfig.json"),
            r#"{"compilerOptions":{}}"#,
        )
        .unwrap();

        // detect: change reported, but no metadata written yet
        let changed = detect_config_change(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        assert!(changed, "new config file should be reported as change");
        assert!(
            store.get_metadata("tsconfig.json_hash").unwrap().is_none(),
            "detect is read-only — hash should NOT be persisted yet"
        );

        // commit: write the new baseline
        commit_config_hashes(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        assert!(
            store.get_metadata("tsconfig.json_hash").unwrap().is_some(),
            "hash should be persisted after commit"
        );
    }

    #[test]
    fn unchanged_config_is_no_change() {
        let (store, tmp) = setup();
        let content = r#"{"compilerOptions":{"baseUrl":"."}}"#;
        fs::write(tmp.path().join("tsconfig.json"), content).unwrap();

        // First run: detect → change → commit
        let changed = detect_config_change(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        assert!(changed);
        commit_config_hashes(&store, tmp.path(), &["tsconfig.json"]).unwrap();

        // Second run with same content: no change
        let changed = detect_config_change(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        assert!(!changed, "unchanged content should not report change");
    }

    #[test]
    fn changed_config_is_change() {
        let (store, tmp) = setup();
        fs::write(tmp.path().join("tsconfig.json"), "v1").unwrap();

        // Establish v1 baseline
        detect_config_change(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        commit_config_hashes(&store, tmp.path(), &["tsconfig.json"]).unwrap();

        fs::write(tmp.path().join("tsconfig.json"), "v2").unwrap();
        let changed = detect_config_change(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        assert!(changed, "modified content should report change");
    }

    #[test]
    fn deleted_config_is_change_and_clears_hash() {
        let (store, tmp) = setup();
        fs::write(tmp.path().join("tsconfig.json"), "v1").unwrap();

        // Establish baseline
        detect_config_change(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        commit_config_hashes(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        assert!(store.get_metadata("tsconfig.json_hash").unwrap().is_some());

        // Delete the config
        fs::remove_file(tmp.path().join("tsconfig.json")).unwrap();
        let changed = detect_config_change(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        assert!(changed, "deleted config should report change");

        // Commit clears the stored hash
        commit_config_hashes(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        assert!(
            store.get_metadata("tsconfig.json_hash").unwrap().is_none(),
            "stored hash should be cleared after commit"
        );
    }

    #[test]
    fn multiple_configs_tracked_independently() {
        let (store, tmp) = setup();
        fs::write(tmp.path().join("a.json"), "a").unwrap();

        // Establish baseline for both
        detect_config_change(&store, tmp.path(), &["a.json", "b.json"]).unwrap();
        commit_config_hashes(&store, tmp.path(), &["a.json", "b.json"]).unwrap();

        // Modify only a.json; b.json still missing both runs
        fs::write(tmp.path().join("a.json"), "a2").unwrap();
        let changed = detect_config_change(&store, tmp.path(), &["a.json", "b.json"]).unwrap();
        assert!(changed, "modified config in set should report change");

        commit_config_hashes(&store, tmp.path(), &["a.json", "b.json"]).unwrap();

        // No further changes
        let changed = detect_config_change(&store, tmp.path(), &["a.json", "b.json"]).unwrap();
        assert!(!changed, "stable multi-config set should report no change");
    }

    /// Regression: double-detect must not skip invalidation.
    ///
    /// Before the two-phase split, the first detect wrote metadata, so the
    /// second detect always saw "no change" — breaking `sync` which calls
    /// `detect_changes()` for display before `sync()` for execution.
    #[test]
    fn double_detect_still_reports_change() {
        let (store, tmp) = setup();
        fs::write(tmp.path().join("tsconfig.json"), "v1").unwrap();

        // First detect (e.g. CLI preview): change reported
        let first = detect_config_change(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        assert!(first, "first detection should report change");

        // Second detect (e.g. sync() internals): STILL change reported
        let second = detect_config_change(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        assert!(
            second,
            "second detection must also report change — metadata was NOT written by first"
        );

        // Metadata still untouched (neither detect writes)
        assert!(store.get_metadata("tsconfig.json_hash").unwrap().is_none());

        // Commit records the baseline
        commit_config_hashes(&store, tmp.path(), &["tsconfig.json"]).unwrap();
        assert!(store.get_metadata("tsconfig.json_hash").unwrap().is_some());
    }
}
