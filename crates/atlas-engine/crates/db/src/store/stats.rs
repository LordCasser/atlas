//! Stats, metadata, schema version, and path resolution.

use rusqlite::params;
use std::path::Path;
use types::*;

use super::{Store, StoreStats};

impl Store {
    // ── Project metadata (key-value) ────────────────────────────────────────

    /// Set a project metadata key-value pair.
    pub fn set_metadata(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO project_metadata (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// Delete a project metadata key.
    pub fn delete_metadata(&self, key: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM project_metadata WHERE key = ?1", params![key])?;
        Ok(())
    }

    /// Get a project metadata value by key.
    pub fn get_metadata(&self, key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare("SELECT value FROM project_metadata WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Return a compact signature for detecting whether indexed graph inputs changed.
    ///
    /// This is intentionally cheap and read-only. It combines core fact counts,
    /// the latest file `index_time`, and index/sync metadata when present.
    pub fn index_signature(&self) -> anyhow::Result<String> {
        let conn = self.lock_read();
        let total_files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let total_symbols: i64 =
            conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        let total_edges: i64 =
            conn.query_row("SELECT COUNT(*) FROM symbol_edges", [], |r| r.get(0))?;
        let total_references: i64 =
            conn.query_row("SELECT COUNT(*) FROM \"references\"", [], |r| r.get(0))?;
        let max_index_time: Option<String> =
            conn.query_row("SELECT MAX(index_time) FROM files", [], |r| r.get(0))?;
        let last_index_time: Option<String> = conn
            .query_row(
                "SELECT value FROM project_metadata WHERE key = 'last_index_time'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| {
                tracing::warn!(?e, "Failed to query last_index_time metadata");
                e
            })
            .ok();
        let last_sync_time: Option<String> = conn
            .query_row(
                "SELECT value FROM project_metadata WHERE key = 'last_sync_time'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| {
                tracing::warn!(?e, "Failed to query last_sync_time metadata");
                e
            })
            .ok();

        Ok(format!(
            "files={total_files};symbols={total_symbols};refs={total_references};edges={total_edges};max_index_time={};last_index_time={};last_sync_time={}",
            max_index_time.unwrap_or_default(),
            last_index_time.unwrap_or_default(),
            last_sync_time.unwrap_or_default(),
        ))
    }

    // ── Stats ───────────────────────────────────────────────────────────────

    /// Returns the total number of indexed files (fast COUNT query).
    pub fn count_files(&self) -> anyhow::Result<usize> {
        let conn = self.lock_read();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        Ok(count as usize)
    }

    /// Collection metrics about the indexed codebase.
    pub fn get_stats(&self) -> anyhow::Result<StoreStats> {
        let conn = self.lock_read();
        let total_files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let total_symbols: i64 =
            conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        let total_edges: i64 =
            conn.query_row("SELECT COUNT(*) FROM symbol_edges", [], |r| r.get(0))?;
        let total_references: i64 =
            conn.query_row("SELECT COUNT(*) FROM \"references\"", [], |r| r.get(0))?;
        let unresolved: i64 = conn.query_row(
            "SELECT COUNT(*) FROM \"references\" WHERE resolved_symbol_id IS NULL",
            [],
            |r| r.get(0),
        )?;
        let sqlite_version: String = conn.query_row("SELECT sqlite_version()", [], |r| r.get(0))?;

        // Symbols grouped by kind
        let mut stmt = conn
            .prepare("SELECT kind, COUNT(*) FROM symbols GROUP BY kind ORDER BY COUNT(*) DESC")?;
        let symbols_by_kind: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(?e, "Symbols-by-kind row decode error, skipping");
                    None
                }
            })
            .collect();

        // Files grouped by language
        let mut stmt = conn.prepare(
            "SELECT language, COUNT(*) FROM files GROUP BY language ORDER BY COUNT(*) DESC",
        )?;
        let files_by_language: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(?e, "Files-by-language row decode error, skipping");
                    None
                }
            })
            .collect();

        Ok(StoreStats {
            total_files,
            total_symbols,
            total_edges,
            total_references,
            unresolved_references: unresolved,
            sqlite_version,
            symbols_by_kind,
            files_by_language,
        })
    }

    // ── Path resolution ────────────────────────────────────────────────────

    /// Resolve a user-facing path to a [`FileId`] using indexed `files.path`.
    ///
    /// Tries exact match on `path = ?`, then suffix match on
    /// `path LIKE '%/' || ?`. Falls back to Rust path normalization for the
    /// suffix case. This replaces the old `list_files()` → linear scan pattern.
    pub fn resolve_file_id(&self, root: &Path, rel_path: &str) -> anyhow::Result<Option<FileId>> {
        let conn = self.lock_read();

        // 1. Exact path match.
        let mut stmt = conn.prepare("SELECT file_id FROM files WHERE path = ?1")?;
        if let Some(row) = stmt.query(params![rel_path])?.next()? {
            return Ok(Some(row.get(0)?));
        }

        // 2. Suffix match (e.g. "helper.ts" matches "src/lib/helper.ts").
        let pattern = format!("%/{rel_path}");
        let mut stmt = conn.prepare(
            "SELECT file_id, path FROM files WHERE path LIKE ?1 ORDER BY path ASC LIMIT 5",
        )?;
        let rows: Vec<_> = stmt
            .query_map(params![&pattern], |row| {
                Ok((row.get::<_, FileId>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(?e, "Path suffix match row decode error, skipping");
                    None
                }
            })
            .collect();

        for (fid, db_path) in &rows {
            if db_path.ends_with(rel_path) {
                return Ok(Some(*fid));
            }
        }

        // 3. Normalized absolute path fallback (for CLI absolute-path queries).
        let normalized = {
            let p = if Path::new(rel_path).is_absolute() {
                rel_path.to_string()
            } else {
                root.join(rel_path).to_string_lossy().to_string()
            };
            if let Ok(stripped) = Path::new(&p).strip_prefix(root) {
                stripped.to_string_lossy().to_string()
            } else {
                p
            }
        };

        if normalized != rel_path {
            let mut stmt = conn.prepare("SELECT file_id FROM files WHERE path = ?1")?;
            if let Some(row) = stmt.query(params![normalized])?.next()? {
                return Ok(Some(row.get(0)?));
            }
        }

        Ok(None)
    }
}
