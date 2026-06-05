//! File CRUD: insert, query, delete files and their associated data.

use rusqlite::params;
use types::*;

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

    /// Return the dominant indexed language for the whole project.
    ///
    /// Ties return `None`; callers should not apply an arbitrary language boost
    /// when the project is evenly mixed.
    pub fn dominant_language(&self) -> anyhow::Result<Option<Language>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT language, COUNT(*) AS n
             FROM files
             GROUP BY language
             ORDER BY n DESC, language",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        dominant_language_from_counts(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Find files whose path starts with the given prefix.
    ///
    /// Uses a SQL `LIKE` query with the prefix escaped for LIKE special chars
    /// to avoid loading all files into memory for O(n) linear scans.
    pub fn find_files_by_path_prefix(&self, prefix: &str) -> anyhow::Result<Vec<FileInfo>> {
        let pattern = format!("{}%", escape_like(prefix));
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT file_id, path, language, content_hash, status
             FROM files WHERE path LIKE ?1 ESCAPE '\\' ORDER BY path",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern], row_to_file_info)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Count indexed files under a user-facing scope.
    ///
    /// Scope is a project-relative directory or file path. Directory scopes
    /// match descendants with `scope/%`; exact file scopes match `scope`.
    pub fn count_files_in_scope(&self, scope: &str) -> anyhow::Result<usize> {
        let normalized = normalize_scope(scope);
        if normalized.is_empty() {
            return self.count_files();
        }
        let (lower, upper) = scope_child_bounds(&normalized);
        let conn = self.lock_read();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM files
             WHERE path = ?1 OR (path >= ?2 AND path < ?3)",
            params![normalized, lower, upper],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Return the dominant indexed language under a user-facing scope.
    ///
    /// Ties return `None` for the same reason as [`Store::dominant_language`].
    pub fn dominant_language_in_scope(&self, scope: &str) -> anyhow::Result<Option<Language>> {
        let normalized = normalize_scope(scope);
        if normalized.is_empty() {
            return self.dominant_language();
        }
        let (lower, upper) = scope_child_bounds(&normalized);
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT language, COUNT(*) AS n
             FROM files
             WHERE path = ?1 OR (path >= ?2 AND path < ?3)
             GROUP BY language
             ORDER BY n DESC, language",
        )?;
        let rows = stmt.query_map(params![normalized, lower, upper], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        dominant_language_from_counts(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Return file IDs under a scope, ordered by path and capped by `limit`.
    pub fn list_file_ids_in_scope(&self, scope: &str, limit: usize) -> anyhow::Result<Vec<FileId>> {
        let normalized = normalize_scope(scope);
        if normalized.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let (lower, upper) = scope_child_bounds(&normalized);
        let conn = self.lock_read();
        let mut stmt = conn.prepare(&format!(
            "SELECT file_id FROM files
             WHERE path = ?1 OR (path >= ?2 AND path < ?3)
             ORDER BY path
             LIMIT {limit}"
        ))?;
        let rows = stmt.query_map(params![normalized, lower, upper], |row| row.get(0))?;
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
        let source_path: String = self.lock_read().query_row(
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
        let pattern = format!("{}%", escape_like(&normalized));

        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT file_id, path, language, content_hash, status
             FROM files WHERE path LIKE ?1 ESCAPE '\\'",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern], row_to_file_info)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

/// Normalize a scope path for database lookups.  `"."` (project root)
/// normalizes to `""` so we count all files.
pub(crate) fn normalize_scope(scope: &str) -> String {
    let s = scope.trim();
    if s == "." {
        return String::new();
    }
    s.trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .replace('\\', "/")
}

pub(crate) fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub(crate) fn scope_child_bounds(scope: &str) -> (String, String) {
    let lower = format!("{scope}/");
    let mut upper = lower.clone();
    upper.push(char::MAX);
    (lower, upper)
}

fn dominant_language_from_counts(rows: Vec<(String, i64)>) -> anyhow::Result<Option<Language>> {
    let Some((lang, top_count)) = rows.first() else {
        return Ok(None);
    };
    if rows.get(1).is_some_and(|(_, count)| count == top_count) {
        return Ok(None);
    }
    Ok(Language::from_str(lang))
}
