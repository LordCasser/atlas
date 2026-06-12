//! File inventory builder — Tier 0 bootstrap population.
//!
//! Populates the `file_inventory` table on first `atlas open` with cheap
//! stat() data (mtime, size, inode, dev).  Content fingerprinting (Tier 0.5)
//! is deferred to a later pass.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs, time};

use anyhow::Result;
use db::Store;
use filesync::discovery::{discover_files, DiscoveryConfig};
use types::ids::FileId;
use types::Language;

/// Populates the file_inventory table on first `atlas open`.
pub struct FileInventoryBuilder {
    store: Arc<Store>,
    project_root: PathBuf,
}

impl FileInventoryBuilder {
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        Self {
            store,
            project_root,
        }
    }

    /// Run discovery and insert all files into file_inventory.
    /// Returns the number of files discovered.
    pub fn populate(&self) -> Result<usize> {
        let config = DiscoveryConfig::default();
        let paths = discover_files(&self.project_root, &config)?;

        let count = paths.len();
        for path in &paths {
            self.insert_file(path)?;
        }
        Ok(count)
    }

    /// Insert a single file into file_inventory using cheap stat().
    fn insert_file(&self, rel_path: &Path) -> Result<()> {
        let abs_path = self.project_root.join(rel_path);
        let metadata = fs::metadata(&abs_path)?;

        let file_id = FileId::generate(&rel_path.to_string_lossy());

        // Language detection from file extension — discovery already filters
        // by known extensions, but we handle None defensively.
        let language = Language::from_path(rel_path).unwrap_or(Language::TypeScript);

        // mtime as seconds since UNIX epoch
        let mtime = metadata
            .modified()?
            .duration_since(time::UNIX_EPOCH)?
            .as_secs() as i64;

        // On Unix, extract inode and dev for cheap change detection
        #[cfg(unix)]
        let (inode, dev) = {
            use std::os::unix::fs::MetadataExt;
            (metadata.ino() as i64, metadata.dev() as i64)
        };
        #[cfg(not(unix))]
        let (inode, dev) = (0i64, 0i64);

        self.store.insert_file_inventory(
            &file_id,
            &rel_path.to_string_lossy(),
            language.as_str(),
            mtime,
            metadata.len() as i64,
            inode,
            dev,
        )?;

        Ok(())
    }
}
