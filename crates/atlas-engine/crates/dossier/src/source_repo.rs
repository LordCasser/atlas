//! Per-request file content cache implementing `SourceRepository` (Decision #2).
//!
//! The `SourceRepo` caches file contents read from disk for the lifetime of a
//! dossier build. Repeated calls to `read_range` / `read_lines` for the same
//! file avoid redundant I/O. The cache is released via `clear_cache()` after
//! the dossier is assembled.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use db::Store;
use types::{FileId, TextRange};

use super::traits::SourceRepository;

/// Per-request source file reader with in-memory caching.
///
/// # Cache lifecycle
///
/// File content is loaded from disk on first access per `FileId` and cached
/// in a `HashMap<String, String>`. The cache is explicitly cleared via
/// [`clear_cache`](SourceRepository::clear_cache) after each dossier build.
pub struct SourceRepo {
    store: Arc<Store>,
    project_root: PathBuf,
    /// Per-request file content cache.  Key: `FileId::to_string()` (hex display).
    /// Interior mutability via `RefCell` because `read_range` / `read_lines`
    /// take `&self` in the trait.
    cache: RefCell<HashMap<String, String>>,
}

impl SourceRepo {
    /// Create a new `SourceRepo` backed by the given store and project root.
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        Self {
            store,
            project_root,
            cache: RefCell::new(HashMap::new()),
        }
    }

    // ── Internal helpers ─────────────────────────────────────────────────

    /// Load file content into cache, reusing the cached value if present.
    ///
    /// Resolves the file path from the store, applies path-traversal safety
    /// checks, reads the file from disk, and inserts it into the cache.
    fn load_file(&self, file_id: &FileId) -> Result<String> {
        let key = file_id.to_string();

        // Fast path: already cached.
        if let Some(content) = self.cache.borrow().get(&key) {
            return Ok(content.clone());
        }

        // Resolve file path from store.
        let file_info = self
            .store
            .get_file(file_id)?
            .ok_or_else(|| anyhow::anyhow!("file not found"))?;

        // Path-traversal safety: canonicalize and verify containment.
        let full_path = self.project_root.join(&file_info.path);
        let canonical = full_path.canonicalize()?;
        let canonical_root = self.project_root.canonicalize()?;
        if !canonical.starts_with(&canonical_root) {
            return Err(anyhow::anyhow!(
                "path traversal warning: '{}' is outside project root",
                file_info.path
            ));
        }

        let content = std::fs::read_to_string(&canonical)?;
        self.cache.borrow_mut().insert(key, content.clone());
        Ok(content)
    }
}

impl SourceRepository for SourceRepo {
    fn read_range(&self, file_id: &FileId, range: &TextRange) -> Result<String> {
        let content = self.load_file(file_id)?;
        let start = range.start_byte as usize;
        let end = range.end_byte as usize;
        let snippet = content.get(start..end).unwrap_or("");
        Ok(snippet.to_string())
    }

    fn read_lines(&self, file_id: &FileId, start_line: u32, end_line: u32) -> Result<String> {
        let content = self.load_file(file_id)?;
        // 1-based inclusive → 0-based start, count of lines.
        let skip = start_line.saturating_sub(1) as usize;
        let take = end_line.saturating_sub(start_line).saturating_add(1) as usize;
        let joined = content
            .lines()
            .skip(skip)
            .take(take)
            .collect::<Vec<&str>>()
            .join("\n");
        Ok(joined)
    }

    fn clear_cache(&mut self) {
        self.cache.borrow_mut().clear();
    }
}
