//! File inventory store — lightweight file discovery index (Tier 0 bootstrap).
//!
//! Populated on first `atlas open` with cheap stat() data. Provides fast
//! file lookup without full extraction.

use rusqlite::params;
use types::ids::FileId;

use super::Store;

impl Store {
    /// Insert or update a file in the inventory (cheap stat, no content hash).
    pub fn insert_file_inventory(
        &self,
        file_id: &FileId,
        path: &str,
        language: &str,
        mtime: i64,
        size: i64,
        inode: i64,
        dev: i64,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO file_inventory
                (file_id, path, language, mtime, size, inode, dev, discovered_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))",
            params![file_id, path, language, mtime, size, inode, dev],
        )?;
        Ok(())
    }

    /// Set the content_hash for a file in inventory (Tier 0.5 fingerprinting).
    pub fn set_file_fingerprint(&self, file_id: &FileId, hash: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE file_inventory SET content_hash = ?1, last_fingerprinted_at = datetime('now')
             WHERE file_id = ?2",
            params![hash, file_id],
        )?;
        Ok(())
    }

    /// Get files that need fingerprinting (no content_hash yet).
    /// Returns (file_id, path) pairs up to `limit`.
    pub fn get_unfingerprinted_files(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<(Vec<u8>, String)>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT file_id, path FROM file_inventory WHERE content_hash IS NULL LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Get files that have been fingerprinted (content_hash IS NOT NULL),
    /// ordered by path for hot-directory-first processing.
    /// Returns (file_id, path) pairs up to `limit`, starting at `offset`.
    pub fn get_fingerprinted_files(
        &self,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<(Vec<u8>, String)>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT file_id, path FROM file_inventory WHERE content_hash IS NOT NULL ORDER BY path LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Get the file_inventory row count.
    pub fn file_inventory_count(&self) -> anyhow::Result<usize> {
        let conn = self.lock_read();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM file_inventory", [], |r| r.get(0))?;
        Ok(count as usize)
    }

    /// Look up a file in inventory by project-relative path.
    pub fn find_file_inventory_by_path(
        &self,
        path: &str,
    ) -> anyhow::Result<Option<FileInventoryRow>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT file_id, path, language, mtime, size, inode, dev, content_hash
             FROM file_inventory WHERE path = ?1",
        )?;
        let result = stmt.query_row(params![path], |row| {
            Ok(FileInventoryRow {
                file_id: row.get(0)?,
                path: row.get(1)?,
                language: row.get(2)?,
                mtime: row.get(3)?,
                size: row.get(4)?,
                inode: row.get(5)?,
                dev: row.get(6)?,
                content_hash: row.get(7)?,
            })
        });
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Look up a file inventory row by FileId.
    pub fn find_file_inventory_by_id(
        &self,
        file_id: &FileId,
    ) -> anyhow::Result<Option<FileInventoryRow>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT file_id, path, language, mtime, size, inode, dev, content_hash
             FROM file_inventory WHERE file_id = ?1",
        )?;
        let result = stmt.query_row(params![file_id], |row| {
            Ok(FileInventoryRow {
                file_id: row.get(0)?,
                path: row.get(1)?,
                language: row.get(2)?,
                mtime: row.get(3)?,
                size: row.get(4)?,
                inode: row.get(5)?,
                dev: row.get(6)?,
                content_hash: row.get(7)?,
            })
        });
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Count inventory files under a user-facing scope.
    pub fn count_file_inventory_in_scope(&self, scope: &str) -> anyhow::Result<usize> {
        let normalized = normalize_inventory_scope(scope);
        let conn = self.lock_read();
        if normalized.is_empty() {
            let count: i64 =
                conn.query_row("SELECT COUNT(*) FROM file_inventory", [], |r| r.get(0))?;
            return Ok(count as usize);
        }
        let (lower, upper) = super::files::scope_child_bounds(&normalized);
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM file_inventory
             WHERE path = ?1 OR (path >= ?2 AND path < ?3)",
            params![normalized, lower, upper],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Return inventory file IDs under a scope, ordered by path and capped by `limit`.
    pub fn list_file_inventory_ids_in_scope(
        &self,
        scope: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<FileId>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let normalized = normalize_inventory_scope(scope);
        let conn = self.lock_read();
        if normalized.is_empty() {
            let mut stmt = conn.prepare(&format!(
                "SELECT file_id FROM file_inventory ORDER BY path LIMIT {limit}"
            ))?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            return rows.collect::<Result<Vec<_>, _>>().map_err(Into::into);
        }
        let (lower, upper) = super::files::scope_child_bounds(&normalized);
        let mut stmt = conn.prepare(&format!(
            "SELECT file_id FROM file_inventory
             WHERE path = ?1 OR (path >= ?2 AND path < ?3)
             ORDER BY path
             LIMIT {limit}"
        ))?;
        let rows = stmt.query_map(params![normalized, lower, upper], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get fingerprinted files that lack manifest extraction state.
    ///
    /// Returns (file_id, path) pairs for files with `content_hash IS NOT NULL`
    /// that have NOT been recorded in `extraction_state` with layer='manifest'
    /// and status='complete'.  Used by Tier 2 bootstrap.
    pub fn get_fingerprinted_files_without_manifest(
        &self,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<(Vec<u8>, String)>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT fi.file_id, fi.path FROM file_inventory fi
             WHERE fi.content_hash IS NOT NULL
               AND fi.file_id NOT IN (
                 SELECT file_id FROM extraction_state
                  WHERE layer = 'manifest' AND status = 'complete'
               )
             ORDER BY fi.path LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Look up a file in inventory by FileId — returns just the path.
    pub fn find_file_inventory_path(&self, file_id: &FileId) -> anyhow::Result<Option<String>> {
        let conn = self.lock_read();
        let result = conn.query_row(
            "SELECT path FROM file_inventory WHERE file_id = ?1",
            params![file_id],
            |row| row.get(0),
        );
        match result {
            Ok(path) => Ok(Some(path)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

fn normalize_inventory_scope(scope: &str) -> String {
    let trimmed = scope
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('/');
    if trimmed == "." {
        String::new()
    } else {
        trimmed.trim_end_matches('/').to_string()
    }
}

/// A row from the file_inventory table.
#[derive(Debug, Clone)]
pub struct FileInventoryRow {
    pub file_id: Vec<u8>,
    pub path: String,
    pub language: String,
    pub mtime: i64,
    pub size: i64,
    pub inode: i64,
    pub dev: i64,
    pub content_hash: Option<String>,
}
