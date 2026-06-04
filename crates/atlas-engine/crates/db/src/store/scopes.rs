//! Scopes and imports: insert + query by file.

use rusqlite::params;
use types::*;

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
            let kind_str: String = row.get(2)?;
            let kind = ScopeKind::from_str(&kind_str).unwrap_or_else(|| {
                tracing::warn!(%kind_str, "Unknown ScopeKind, defaulting to default");
                Default::default()
            });
            Ok(ScopeDef {
                id: row.get(0)?,
                file_id: row.get(1)?,
                kind,
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
            let kind_str: String = row.get(2)?;
            let kind = ImportKind::from_str(&kind_str).unwrap_or_else(|| {
                tracing::warn!(%kind_str, "Unknown ImportKind, defaulting to default");
                Default::default()
            });
            Ok(ImportDef {
                id: row.get(0)?,
                file_id: row.get(1)?,
                kind,
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
    ///
    /// # C/C++ `#include` resolution
    ///
    /// For languages where imports use bare filenames (e.g., `#include "helper.h"`),
    /// the standard path-substring LIKE query fails because the `module` column
    /// stores just `helper.h` rather than the full path `src/helper.h`.  This
    /// method includes a C/C++ include resolution pass that:
    ///
    /// 1. Extracts the target file's basename and matches it against import modules.
    /// 2. Resolves relative includes by combining the importing file's directory
    ///    with the include path.
    ///
    /// Both results are merged and returned.
    pub fn find_dependents_by_file(
        &self,
        file_id: &FileId,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let conn = self.lock_read();

        // Get the target file's path for matching
        let target_path: String = conn.query_row(
            "SELECT path FROM files WHERE file_id = ?1",
            params![file_id],
            |row| row.get(0),
        )?;

        // ── Path A: Standard path-substring LIKE query ──────────────────
        // Works for TypeScript, Python, Java etc. where module stores
        // relative paths like "./foo/bar" or "react".
        let mut results: Vec<(String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT f.path, i.module
                 FROM imports i
                 JOIN files f ON f.file_id = i.file_id
                 WHERE i.module LIKE ?1
                 ORDER BY f.path",
            )?;
            let pattern = format!("%{target_path}%");
            stmt.query_map(params![pattern], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(
                        ?e,
                        "Dependent import row decode error (LIKE path), skipping"
                    );
                    None
                }
            })
            .collect()
        };

        if !results.is_empty() {
            return Ok(results);
        }

        // ── Path B: C/C++ include resolution ────────────────────────────
        // Extract the target file's basename for bare-filename matching.
        let target_basename = if let Some(pos) = target_path.rfind('/') {
            &target_path[pos + 1..]
        } else {
            &target_path
        };

        // Collect include imports that might reference this file.
        // We need both bare-filename matches and relative-path matches.
        {
            let mut stmt = conn.prepare(
                "SELECT f.file_id, f.path, i.module
                 FROM imports i
                 JOIN files f ON f.file_id = i.file_id
                 WHERE i.kind = 'include'
                   AND i.is_relative = 1
                 ORDER BY f.path",
            )?;
            let candidate_rows: Vec<(FileId, String, String)> = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, FileId>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .filter_map(|r| match r {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::warn!(?e, "Include-import row decode error, skipping");
                        None
                    }
                })
                .collect();

            for (_importing_fid, importing_path, module) in candidate_rows {
                // Strategy 1: Bare basename match — `helper.h` == `helper.h`
                if module == target_basename {
                    results.push((importing_path, module));
                    continue;
                }

                // Strategy 2: Relative include path resolved against importing
                // file's directory.  e.g., importing `src/main.c` with
                // `#include "helper.h"` → check `src/helper.h`.
                if let Some(parent_dir) = std::path::Path::new(&importing_path).parent() {
                    let resolved = parent_dir.join(&module);
                    if resolved.to_string_lossy() == target_path {
                        results.push((importing_path, module));
                    }
                }
            }
        }

        Ok(results)
    }
}
