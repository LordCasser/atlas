//! File CRUD: insert, query, delete files and their associated data.

use types::*;
use rusqlite::params;

use super::Store;
use crate::store_rows::row_to_file_info;

impl Store {
    /// Insert or update a file record.
    pub fn upsert_file(&self, file: &FileInfo) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            r#"INSERT OR REPLACE INTO files
               (file_id, path, language, content_hash, status, index_time)
               VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))"#,
            params![
                file.file_id,
                file.path,
                file.language.as_str(),
                file.content_hash,
                file.status.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Delete all data associated with a file (FOREIGN KEY CASCADE handles most).
    pub fn delete_file_data(&self, file_id: &FileId) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM files WHERE file_id = ?1", params![file_id])?;
        Ok(())
    }

    /// Delete all data for multiple files in a single transaction.
    ///
    /// Uses the store's `with_transaction` for RAII rollback — if any
    /// `DELETE` fails, the entire batch is rolled back automatically.
    ///
    /// Used before re-indexing modified files to ensure stale rows
    /// (symbols, references, dataflow, CFG, etc.) are removed atomically.
    pub fn delete_files_batch(&self, file_ids: &[FileId]) -> anyhow::Result<()> {
        self.with_transaction(|tx| {
            for file_id in file_ids {
                tx.execute("DELETE FROM files WHERE file_id = ?1", params![file_id])?;
            }
            Ok(())
        })
    }

    /// Get file info by ID.
    pub fn get_file(&self, file_id: &FileId) -> anyhow::Result<Option<FileInfo>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT file_id, path, language, content_hash, status
             FROM files WHERE file_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![file_id], row_to_file_info)?;
        match rows.next() {
            Some(Ok(f)) => Ok(Some(f)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// List all indexed files.
    pub fn list_files(&self) -> anyhow::Result<Vec<FileInfo>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT file_id, path, language, content_hash, status FROM files ORDER BY path",
        )?;
        let rows = stmt.query_map([], row_to_file_info)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Resolve a relative module path against a source file to find matching indexed files.
    ///
    /// For barrel re-export chain walking: given `src/barrel/index.ts` and `./lib`,
    /// resolves to files matching `src/barrel/lib%` (handles `lib.ts`, `lib/index.ts`, etc.).
    /// Handles `../` parent directory traversal.
    pub fn resolve_relative_module(
        &self,
        source_file_id: &FileId,
        relative_module: &str,
    ) -> anyhow::Result<Vec<FileInfo>> {
        // Get source file path
        let source_path: String = self
            .lock_read()
            .query_row(
                "SELECT path FROM files WHERE file_id = ?1",
                rusqlite::params![source_file_id],
                |row| row.get(0),
            )?;

        // Extract directory from source path
        let source_dir = std::path::Path::new(&source_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        // Resolve relative path against source directory
        let resolved = std::path::Path::new(&source_dir)
            .join(relative_module)
            .to_string_lossy()
            .to_string();

        // Normalize and build LIKE pattern (handles extension variations:
        // "lib" → matches "lib.ts", "lib/index.ts", "lib.js", etc.)
        let normalized = resolved.replace('\\', "/");
        let pattern = format!("{}%", normalized);

        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT file_id, path, language, content_hash, status
             FROM files WHERE path LIKE ?1 ESCAPE '\\'",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern], row_to_file_info)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
