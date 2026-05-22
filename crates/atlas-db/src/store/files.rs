//! File CRUD: insert, query, delete files and their associated data.

use atlas_types::*;
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
        let conn = self.lock();
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
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT file_id, path, language, content_hash, status FROM files ORDER BY path",
        )?;
        let rows = stmt.query_map([], row_to_file_info)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
