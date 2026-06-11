//! Stale index cleanup helpers.
//!
//! Re-indexing a file invalidates both incoming references to that file's old
//! symbols and outgoing graph edges derived from that file's old references.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use db::Store;
use tracing::debug_span;
use types::FileId;
use workspace::SourcePath;

/// Convert a project-relative path into the FileId used by extraction.
pub fn source_file_id(path: &Path) -> Result<FileId> {
    let source_path = SourcePath::try_from_relative(&path.to_string_lossy())?;
    Ok(FileId::generate(source_path.as_str()))
}

/// Clean stale facts for project-relative file paths.
pub fn clean_stale_file_paths(store: &Arc<Store>, paths: &[PathBuf]) -> Result<Vec<FileId>> {
    let file_ids: Vec<FileId> = paths
        .iter()
        .map(|path| source_file_id(path))
        .collect::<Result<Vec<_>>>()?;
    clean_stale_file_ids(store, &file_ids)?;
    Ok(file_ids)
}

/// Clean stale facts for file IDs before deleting or replacing file facts.
///
/// Delegates to `Store::clean_stale_file_facts` which wraps all operations
/// in a single transaction for atomicity.
pub fn clean_stale_file_ids(store: &Arc<Store>, file_ids: &[FileId]) -> Result<()> {
    if file_ids.is_empty() {
        return Ok(());
    }
    let _span = debug_span!(target: "atlas_sync", "sync.incremental.cleanup", dirty_count = file_ids.len()).entered();
    store.clean_stale_file_facts(file_ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_file_id_rejects_absolute_paths() {
        assert!(source_file_id(Path::new("/tmp/main.ts")).is_err());
    }
}
