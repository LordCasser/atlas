//! Scopes and imports: insert + query by file.

use types::*;
use rusqlite::params;

use super::Store;
use crate::store_writers::{write_imports, write_scopes};

impl Store {
    // ── Scopes ──────────────────────────────────────────────────────────────

    /// Batch-insert scopes.
    pub fn insert_scopes(&self, scopes: &[ScopeDef]) -> anyhow::Result<()> {
        if scopes.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_scopes(tx, scopes))
    }

    /// Find all scopes for a file.
    pub fn find_scopes_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<ScopeDef>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT scope_id, file_id, kind, name, scope_path, parent_id,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM scopes WHERE file_id = ?1",
        )?;
        let rows = stmt.query_map(params![file_id], |row| {
            Ok(ScopeDef {
                id: row.get(0)?,
                file_id: row.get(1)?,
                kind: ScopeKind::from_str(row.get::<_, String>(2)?.as_str()).unwrap_or_default(),
                name: row.get(3)?,
                scope_path: row.get(4)?,
                parent_id: row.get(5)?,
                range: TextRange {
                    start_byte: row.get(6)?,
                    end_byte: row.get(7)?,
                    start_line: row.get(8)?,
                    start_column: row.get(9)?,
                    end_line: row.get(10)?,
                    end_column: row.get(11)?,
                },
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // ── Imports ─────────────────────────────────────────────────────────────

    /// Batch-insert imports.
    pub fn insert_imports(&self, imports: &[ImportDef]) -> anyhow::Result<()> {
        if imports.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_imports(tx, imports))
    }

    /// Find all imports for a file.
    pub fn find_imports_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<ImportDef>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT import_id, file_id, kind, module, imported_name, local_name,
                    is_wildcard, is_relative, alias,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM imports WHERE file_id = ?1",
        )?;
        let rows = stmt.query_map(params![file_id], |row| {
            Ok(ImportDef {
                id: row.get(0)?,
                file_id: row.get(1)?,
                kind: ImportKind::from_str(row.get::<_, String>(2)?.as_str()).unwrap_or_default(),
                module: row.get(3)?,
                imported_name: row.get(4)?,
                local_name: row.get(5)?,
                is_wildcard: row.get(6)?,
                is_relative: row.get(7)?,
                alias: row.get(8)?,
                range: TextRange {
                    start_byte: row.get(9)?,
                    end_byte: row.get(10)?,
                    start_line: row.get(11)?,
                    start_column: row.get(12)?,
                    end_line: row.get(13)?,
                    end_column: row.get(14)?,
                },
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find files that import a given file (reverse dependencies / dependents).
    ///
    /// Returns tuples of (importing_file_path, import_module_string).
    /// This is a best-effort O(N) scan over all imports; for large projects
    /// consider building an in-memory dependency index.
    pub fn find_dependents_by_file(
        &self,
        file_id: &FileId,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let conn = self.lock_read();
        // Get the target file's path for display
        let target_path: String = conn.query_row(
            "SELECT path FROM files WHERE file_id = ?1",
            params![file_id],
            |row| row.get(0),
        )?;

        // Find all imports whose module references this file
        let mut stmt = conn.prepare(
            "SELECT f.path, i.module, i.kind
             FROM imports i
             JOIN files f ON f.file_id = i.file_id
             WHERE i.module LIKE ?1 OR i.module LIKE ?2
             ORDER BY f.path",
        )?;
        let pattern_rel = format!("%{}%", target_path);
        let rows = stmt.query_map(
            params![pattern_rel, pattern_rel],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
