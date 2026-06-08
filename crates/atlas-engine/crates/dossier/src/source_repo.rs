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

    fn clear_cache(&self) {
        self.cache.borrow_mut().clear();
    }
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use types::{FileId, FileInfo, Language, ParseStatus, TextRange};

    /// Create an in-memory Store with schema initialized.
    fn make_store() -> Arc<Store> {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        Arc::new(store)
    }

    /// Insert a FileInfo into the store.
    fn seed_file(store: &Store, file_id: FileId, path: &str) {
        store
            .upsert_file(&FileInfo {
                file_id,
                path: path.to_string(),
                language: Language::TypeScript,
                content_hash: "abc".to_string(),
                status: ParseStatus::Success,
            })
            .unwrap();
    }

    #[test]
    fn read_lines_reads_correct_content() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.ts");
        std::fs::write(&file_path, "line1\nline2\nline3\nline4\nline5\n").unwrap();

        let store = make_store();
        let file_id = FileId::generate("test.ts");
        seed_file(&store, file_id, "test.ts");

        let repo = SourceRepo::new(store, dir.path().to_path_buf());
        let lines = repo.read_lines(&file_id, 2, 4).unwrap();
        assert_eq!(lines, "line2\nline3\nline4");
    }

    #[test]
    fn read_range_reads_correct_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let content = "fn hello() {\n    println!(\"hi\");\n}\n";
        let file_path = dir.path().join("main.rs");
        std::fs::write(&file_path, content).unwrap();

        let store = make_store();
        let file_id = FileId::generate("main.rs");
        seed_file(&store, file_id, "main.rs");

        let repo = SourceRepo::new(store, dir.path().to_path_buf());
        let range = TextRange {
            start_byte: 3,
            end_byte: 8,
            ..Default::default()
        };
        let snippet = repo.read_range(&file_id, &range).unwrap();
        assert_eq!(snippet, "hello");
    }

    #[test]
    fn caching_two_reads_only_hit_disk_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "cached\n").unwrap();

        let store = make_store();
        let file_id = FileId::generate("f.txt");
        seed_file(&store, file_id, "f.txt");

        let repo = SourceRepo::new(store, dir.path().to_path_buf());

        // First read — loaded from disk.
        let r1 = repo.read_lines(&file_id, 1, 1).unwrap();
        assert_eq!(r1, "cached");

        // Modify the file on disk.
        std::fs::write(dir.path().join("f.txt"), "modified\n").unwrap();

        // Second read — still cached, returns original content.
        let r2 = repo.read_lines(&file_id, 1, 1).unwrap();
        assert_eq!(r2, "cached");
    }

    #[test]
    fn path_traversal_outside_project_root_fails() {
        let dir = tempfile::tempdir().unwrap();
        // Create a file in the *parent* of the tempdir so that `../secret.txt`
        // relative to `dir` can canonicalize to an existing file outside.
        let parent = dir.path().parent().unwrap().to_path_buf();
        std::fs::write(parent.join("secret.txt"), "secret\n").unwrap();

        let store = make_store();
        let file_id = FileId::generate("../secret.txt");
        seed_file(&store, file_id, "../secret.txt");

        let repo = SourceRepo::new(store, dir.path().to_path_buf());
        let result = repo.read_lines(&file_id, 1, 1);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("outside project root"),
            "expected path-traversal rejection, got: {err_msg}"
        );
    }

    #[test]
    fn clear_cache_flushes_and_subsequent_read_reloads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("g.txt"), "v1\n").unwrap();

        let store = make_store();
        let file_id = FileId::generate("g.txt");
        seed_file(&store, file_id, "g.txt");

        let repo = SourceRepo::new(store, dir.path().to_path_buf());

        // Prime cache.
        let r1 = repo.read_lines(&file_id, 1, 1).unwrap();
        assert_eq!(r1, "v1");

        // Clear cache and modify file.
        repo.clear_cache();
        std::fs::write(dir.path().join("g.txt"), "v2\n").unwrap();

        // Re-read — must see new content.
        let r2 = repo.read_lines(&file_id, 1, 1).unwrap();
        assert_eq!(r2, "v2");
    }
}
