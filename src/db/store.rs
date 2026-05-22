//! Atlas Store — the single SQLite persistence layer.
//!
//! `Store` wraps a `Mutex<Connection>` and provides all CRUD operations.
//! For MVP, a single writer/reader suffices. Future: split into `StoreWriter`
//! and `StoreReader` for concurrent read access.

use crate::db::schema::{CURRENT_SCHEMA_VERSION, SCHEMA_DDL};

use crate::types::*;
use rusqlite::{Connection, Transaction, params};
use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::store_fts::{chrono_now_ms, is_process_alive, sanitize_fts5_query};
use super::store_rows::*;
use super::store_writers::*;

// ---------------------------------------------------------------------------
// StoreReader — read-only query interface
// ---------------------------------------------------------------------------

/// Read-only query interface backed by a shared SQLite connection.
///
/// All methods take `&self` and perform only SELECT queries.
/// For mutations, use `Store` which derefs to `StoreReader`.
pub struct StoreReader {
    pub(crate) conn: Mutex<Connection>,
}

impl StoreReader {
    /// Lock the underlying SQLite connection for read access.
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Find a symbol by its deterministic SymbolId.
    pub fn find_symbol_by_id(&self, id: &SymbolId) -> anyhow::Result<Option<SymbolDef>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json
             FROM symbols WHERE symbol_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], row_to_symbol)?;
        match rows.next() {
            Some(Ok(s)) => Ok(Some(s)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Batch-lookup symbols by IDs in a single query.
    pub fn find_symbols_by_ids(&self, ids: &[SymbolId]) -> anyhow::Result<Vec<SymbolDef>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json
             FROM symbols WHERE symbol_id IN ({})",
            placeholders.join(","),
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> =
            ids.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
        let rows = stmt.query_map(params.as_slice(), row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

// ---------------------------------------------------------------------------
// Store — read/write persistence layer
// ---------------------------------------------------------------------------

/// Thread-safe SQLite persistence layer.
///
/// Derefs to `StoreReader` for foundational read operations (`find_symbol_by_id`,
/// `find_symbols_by_ids`).  Most query methods still live on `Store` directly;
/// the full split into `StoreWriter` / `StoreReader` is deferred to crate
/// workspace restructuring (Item 10).
pub struct Store {
    reader: StoreReader,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl Deref for Store {
    type Target = StoreReader;

    fn deref(&self) -> &Self::Target {
        &self.reader
    }
}

impl Store {
    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Lock the underlying SQLite connection for read or write access.
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.reader.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    /// Open or create the Atlas database in `project_root/.atlas/`.
    pub fn open(project_root: &Path) -> anyhow::Result<Self> {
        let atlas_dir = project_root.join(".atlas");
        std::fs::create_dir_all(&atlas_dir)?;

        let db_path = atlas_dir.join("atlas.db");
        let conn = Connection::open(&db_path)?;

        // Performance pragmas
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            PRAGMA cache_size = -20000; -- ~20 MB
            "#,
        )?;

        Ok(Self {
            reader: StoreReader {
                conn: Mutex::new(conn),
            },
            db_path,
        })
    }

    /// Open in-memory (for tests).
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self {
            reader: StoreReader {
                conn: Mutex::new(conn),
            },
            db_path: PathBuf::from(":memory:"),
        })
    }

    /// Initialize the schema (idempotent).
    pub fn init_schema(&self) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute_batch(SCHEMA_DDL)?;

        // Record current version (always V1 during rapid development)
        conn.execute(
            "INSERT OR IGNORE INTO schema_versions (version, description)
             VALUES (?1, ?2)",
            params![CURRENT_SCHEMA_VERSION, "v1: initial schema"],
        )?;

        Ok(())
    }

    /// Find the project root by walking up from `cwd` looking for `.atlas/`.
    pub fn find_project_root() -> Option<PathBuf> {
        let cwd = std::env::current_dir().ok()?;
        let mut current = cwd.as_path();
        loop {
            if current.join(".atlas").is_dir() {
                return Some(current.to_path_buf());
            }
            current = current.parent()?;
        }
    }

    // -----------------------------------------------------------------------
    // Transaction helpers
    // -----------------------------------------------------------------------

    /// Run a closure inside a transaction.
    fn with_transaction<F, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&Transaction) -> anyhow::Result<T>,
    {
        let conn = self.lock();
        let tx = conn.unchecked_transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // Exclusive lock (cross-process, via project_metadata table)
    // -----------------------------------------------------------------------

    /// Try to acquire an exclusive write lock.
    ///
    /// Records the current PID and timestamp in `project_metadata`.
    /// Fails if another process already holds the lock and is still alive.
    /// Stale locks (process died) are automatically stolen.
    pub fn acquire_exclusive_lock(&self) -> anyhow::Result<()> {
        let conn = self.lock();
        let pid = std::process::id();
        let now = chrono_now_ms();

        // Check for existing lock
        let existing: Option<(i64, i64)> = conn
            .query_row(
                "SELECT value FROM project_metadata WHERE key = 'exclusive_lock_pid'",
                [],
                |row| {
                    let v: String = row.get(0)?;
                    // Format: "pid:timestamp_ms"
                    let parts: Vec<&str> = v.splitn(2, ':').collect();
                    if parts.len() == 2 {
                        Ok(Some((
                            parts[0].parse().unwrap_or(0),
                            parts[1].parse().unwrap_or(0),
                        )))
                    } else {
                        Ok(None)
                    }
                },
            )
            .ok()
            .flatten();

        if let Some((existing_pid, _ts)) = existing {
            if existing_pid != pid as i64 && is_process_alive(existing_pid) {
                anyhow::bail!(
                    "Another atlas process (PID {}) already holds the lock",
                    existing_pid
                );
            }
            // Stale lock — steal it
        }

        // Write our lock
        let lock_value = format!("{}:{}", pid, now);
        conn.execute(
            "INSERT OR REPLACE INTO project_metadata (key, value) VALUES ('exclusive_lock_pid', ?1)",
            params![lock_value],
        )?;
        Ok(())
    }

    /// Release the exclusive write lock.
    ///
    /// Only releases if the current PID matches the lock holder.
    pub fn release_exclusive_lock(&self) -> anyhow::Result<()> {
        let conn = self.lock();
        let pid = std::process::id();
        let existing: Option<i64> = conn
            .query_row(
                "SELECT value FROM project_metadata WHERE key = 'exclusive_lock_pid'",
                [],
                |row| {
                    let v: String = row.get(0)?;
                    Ok(v.splitn(2, ':').next().and_then(|s| s.parse().ok()))
                },
            )
            .ok()
            .flatten();

        if let Some(existing_pid) = existing {
            if existing_pid == pid as i64 {
                conn.execute(
                    "DELETE FROM project_metadata WHERE key = 'exclusive_lock_pid'",
                    [],
                )?;
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Files
    // -----------------------------------------------------------------------

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
    /// Used before re-indexing modified files to ensure stale rows
    /// (symbols, references, dataflow, CFG, etc.) are removed atomically.
    pub fn delete_files_batch(&self, file_ids: &[FileId]) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        for file_id in file_ids {
            conn.execute("DELETE FROM files WHERE file_id = ?1", params![file_id])?;
        }
        conn.execute_batch("COMMIT")?;
        Ok(())
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

    // -----------------------------------------------------------------------
    // Symbols
    // -----------------------------------------------------------------------

    /// Batch-insert symbols inside a transaction.
    pub fn insert_symbols(&self, symbols: &[SymbolDef]) -> anyhow::Result<()> {
        if symbols.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_symbols(tx, symbols))
    }

    /// Find all symbols in a file.
    pub fn find_symbols_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json
             FROM symbols WHERE file_id = ?1 ORDER BY qualified_name",
        )?;
        let rows = stmt.query_map(params![file_id], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// FTS5 search by name (default limit 50).
    pub fn search_symbols(&self, query: &str) -> anyhow::Result<Vec<SymbolDef>> {
        self.search_symbols_with_limit(query, 50, None)
    }

    /// FTS5 search by name with custom limit and optional kind filter.
    ///
    /// When `kind_filter` is provided, the filter is applied at the SQL level
    /// (`WHERE s.kind = ?`) so that the search pipeline's fallback logic
    /// (FTS5 → LIKE → Levenshtein) correctly triggers when a stage returns
    /// zero results after filtering.
    pub fn search_symbols_with_limit(
        &self,
        query: &str,
        limit: usize,
        kind_filter: Option<&SymbolKind>,
    ) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.lock();
        let safe_query = sanitize_fts5_query(query);
        if safe_query.is_empty() {
            return Ok(Vec::new());
        }
        // Append * for prefix matching (matches "User" → "UserManager")
        let match_query = format!("{}*", safe_query);
        let sql = if kind_filter.is_some() {
            format!(
                r#"SELECT s.symbol_id, s.file_id, s.kind, s.name, s.qualified_name,
                          s.symbol_path_json, s.language,
                          s.range_start_byte, s.range_end_byte, s.range_start_line,
                          s.range_start_column, s.range_end_line, s.range_end_column,
                          s.name_start_byte, s.name_end_byte, s.name_start_line,
                          s.name_start_column, s.name_end_line, s.name_end_column,
                          s.signature, s.visibility, s.exported, s.static_, s.async_,
                          s.container_id, s.scope_id, s.package_name, s.namespace_path_json
                   FROM symbols s
                   JOIN symbols_fts fts ON s.rowid = fts.rowid
                   WHERE symbols_fts MATCH ?1 AND s.kind = ?2
                   ORDER BY rank
                   LIMIT {}"#,
                limit
            )
        } else {
            format!(
                r#"SELECT s.symbol_id, s.file_id, s.kind, s.name, s.qualified_name,
                          s.symbol_path_json, s.language,
                          s.range_start_byte, s.range_end_byte, s.range_start_line,
                          s.range_start_column, s.range_end_line, s.range_end_column,
                          s.name_start_byte, s.name_end_byte, s.name_start_line,
                          s.name_start_column, s.name_end_line, s.name_end_column,
                          s.signature, s.visibility, s.exported, s.static_, s.async_,
                          s.container_id, s.scope_id, s.package_name, s.namespace_path_json
                   FROM symbols s
                   JOIN symbols_fts fts ON s.rowid = fts.rowid
                   WHERE symbols_fts MATCH ?1
                   ORDER BY rank
                   LIMIT {}"#,
                limit
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = if let Some(kind) = kind_filter {
            let kind_str = kind.as_str();
            stmt.query_map(params![match_query, kind_str], row_to_symbol)?
        } else {
            stmt.query_map(params![match_query], row_to_symbol)?
        };
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// LIKE-based name search (fallback when FTS5 returns nothing).
    ///
    /// Optional `kind_filter` is applied at the SQL level so that the
    /// search pipeline's fallback logic works correctly with kind filters.
    pub fn search_symbols_by_name_like(
        &self,
        pattern: &str,
        language: Option<&Language>,
        limit: usize,
        kind_filter: Option<&SymbolKind>,
    ) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.lock();
        let like_pattern = format!("%{}%", pattern.replace('%', "").replace('_', ""));
        if like_pattern.len() <= 2 {
            // Just "%%"
            return Ok(Vec::new());
        }

        // Build WHERE clause dynamically based on filters
        let mut where_clauses = vec!["(s.name LIKE ?1 OR s.qualified_name LIKE ?2)".to_string()];
        let mut param_idx = 3; // ?1 and ?2 are LIKE patterns

        if language.is_some() {
            where_clauses.push(format!("s.language = ?{}", param_idx));
            param_idx += 1;
        }
        if kind_filter.is_some() {
            where_clauses.push(format!("s.kind = ?{}", param_idx));
        }

        let where_sql = where_clauses.join(" AND ");

        let sql = format!(
            r#"SELECT s.symbol_id, s.file_id, s.kind, s.name, s.qualified_name,
                      s.symbol_path_json, s.language,
                      s.range_start_byte, s.range_end_byte, s.range_start_line,
                      s.range_start_column, s.range_end_line, s.range_end_column,
                      s.name_start_byte, s.name_end_byte, s.name_start_line,
                      s.name_start_column, s.name_end_line, s.name_end_column,
                      s.signature, s.visibility, s.exported, s.static_, s.async_,
                      s.container_id, s.scope_id, s.package_name, s.namespace_path_json
               FROM symbols s
               WHERE {}
               LIMIT {}"#,
            where_sql, limit
        );

        let mut stmt = conn.prepare(&sql)?;

        // Build params dynamically
        let lang_str = language.map(|l| l.as_str().to_string()).unwrap_or_default();
        let kind_str = kind_filter
            .map(|k| k.as_str().to_string())
            .unwrap_or_default();

        let rows = match (language.is_some(), kind_filter.is_some()) {
            (true, true) => stmt.query_map(
                params![like_pattern, like_pattern, lang_str, kind_str],
                row_to_symbol,
            )?,
            (true, false) => {
                stmt.query_map(params![like_pattern, like_pattern, lang_str], row_to_symbol)?
            }
            (false, true) => {
                stmt.query_map(params![like_pattern, like_pattern, kind_str], row_to_symbol)?
            }
            (false, false) => stmt.query_map(params![like_pattern, like_pattern], row_to_symbol)?,
        };
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Counts
    // -----------------------------------------------------------------------

    /// Total number of symbols in the database.
    pub fn count_symbols(&self) -> anyhow::Result<usize> {
        let conn = self.lock();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    // -----------------------------------------------------------------------
    // References
    // -----------------------------------------------------------------------

    /// Batch-insert references inside a transaction.
    pub fn insert_references(&self, refs: &[ReferenceUse]) -> anyhow::Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_references(tx, refs))
    }

    /// Find all references belonging to a file.
    pub fn find_references_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<ReferenceUse>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(REFERENCE_SELECT_WHERE)?;
        let rows = stmt.query_map(params![file_id], row_to_reference)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find unresolved references (no resolved target).
    pub fn find_unresolved_references(&self) -> anyhow::Result<Vec<ReferenceUse>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{} WHERE resolved_symbol_id IS NULL",
            REFERENCE_SELECT_NO_WHERE
        ))?;
        let rows = stmt.query_map([], row_to_reference)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Update the resolved target of a specific reference.
    pub fn update_reference_resolution(
        &self,
        reference_id: &ReferenceId,
        target: &ResolvedTarget,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE \"references\" SET
                resolved_symbol_id = ?2,
                resolved_confidence = ?3,
                resolved_strategy = ?4,
                resolved_provenance = ?5
             WHERE reference_id = ?1",
            params![
                reference_id,
                target.symbol_id,
                target.confidence.as_f32(),
                target.strategy.as_str(),
                target.provenance.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Batch-update resolved targets for multiple references in a single transaction.
    ///
    /// This is significantly faster than calling `update_reference_resolution` per-reference
    /// because it amortizes the transaction overhead.
    pub fn batch_update_resolutions(
        &self,
        resolutions: &[(ReferenceId, ResolvedTarget)],
    ) -> anyhow::Result<()> {
        if resolutions.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| {
            let mut stmt = tx.prepare(
                "UPDATE \"references\" SET
                    resolved_symbol_id = ?2,
                    resolved_confidence = ?3,
                    resolved_strategy = ?4,
                    resolved_provenance = ?5
                 WHERE reference_id = ?1",
            )?;
            for (ref_id, target) in resolutions {
                stmt.execute(params![
                    ref_id,
                    target.symbol_id,
                    target.confidence.as_f32(),
                    target.strategy.as_str(),
                    target.provenance.as_str(),
                ])?;
            }
            Ok(())
        })
    }

    /// Batch-insert edges inside a transaction (re-export with explicit name).
    ///
    /// This is the same as `insert_edges` but named for clarity in the
    /// resolution pipeline where we accumulate edges and flush them in batches.
    pub fn batch_insert_edges(&self, edges: &[RawEdge]) -> anyhow::Result<()> {
        self.insert_edges(edges)
    }

    // -----------------------------------------------------------------------
    // Resolved fact invalidation (P2)
    // -----------------------------------------------------------------------

    /// Clear all resolution results for references belonging to a file.
    ///
    /// This is called when a file is modified — the references themselves
    /// remain (they are never deleted), but their resolved targets become
    /// stale and must be re-computed.
    ///
    /// Returns the number of references that were invalidated.
    pub fn invalidate_references_for_file(&self, file_id: &FileId) -> anyhow::Result<usize> {
        let conn = self.lock();
        let count = conn.execute(
            r#"UPDATE "references" SET
                resolved_symbol_id = NULL,
                resolved_confidence = NULL,
                resolved_strategy = NULL,
                resolved_provenance = NULL
               WHERE file_id = ?1 AND resolved_symbol_id IS NOT NULL"#,
            params![file_id],
        )?;
        Ok(count)
    }

    /// Delete all edges that were created from references belonging to a file.
    ///
    /// When a file is modified, the edges derived from its references become
    /// invalid. This deletes edges whose `ref_id` points to a reference in
    /// the given file.
    ///
    /// Returns the number of edges deleted.
    pub fn delete_edges_for_file_references(&self, file_id: &FileId) -> anyhow::Result<usize> {
        let conn = self.lock();
        // Find all reference IDs belonging to this file, then delete edges
        // whose ref_id matches any of them.
        let count = conn.execute(
            r#"DELETE FROM symbol_edges WHERE ref_id IN (
                SELECT reference_id FROM "references" WHERE file_id = ?1
            )"#,
            params![file_id],
        )?;
        Ok(count)
    }

    /// Invalidate ALL resolved references (clear resolution columns).
    ///
    /// Used when project-level configuration (e.g. tsconfig.json) changes,
    /// which can affect import resolution across all files.
    ///
    /// Returns the number of references invalidated.
    pub fn invalidate_all_references(&self) -> anyhow::Result<usize> {
        let conn = self.lock();
        let count = conn.execute(
            r#"UPDATE "references" SET
                resolved_symbol_id = NULL,
                resolved_confidence = NULL,
                resolved_strategy = NULL,
                resolved_provenance = NULL
             WHERE resolved_symbol_id IS NOT NULL"#,
            [],
        )?;
        Ok(count)
    }

    /// Delete ALL edges from the symbol graph.
    ///
    /// Used together with `invalidate_all_references` when project configuration
    /// changes require a full re-resolution and edge rebuild.
    ///
    /// Returns the number of edges deleted.
    pub fn delete_all_edges(&self) -> anyhow::Result<usize> {
        let conn = self.lock();
        let count = conn.execute("DELETE FROM symbol_edges", [])?;
        Ok(count)
    }

    // -----------------------------------------------------------------------
    // Scopes
    // -----------------------------------------------------------------------

    /// Batch-insert scopes.
    pub fn insert_scopes(&self, scopes: &[ScopeDef]) -> anyhow::Result<()> {
        if scopes.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_scopes(tx, scopes))
    }

    // -----------------------------------------------------------------------
    // Imports
    // -----------------------------------------------------------------------

    /// Batch-insert imports.
    pub fn insert_imports(&self, imports: &[ImportDef]) -> anyhow::Result<()> {
        if imports.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_imports(tx, imports))
    }

    /// Batch-insert edges inside a transaction.
    pub fn insert_edges(&self, edges: &[RawEdge]) -> anyhow::Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_edges(tx, edges))
    }

    /// Batch-insert callsites inside a transaction.
    pub fn insert_callsites(&self, callsites: &[Callsite]) -> anyhow::Result<()> {
        if callsites.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_callsites(tx, callsites))
    }

    /// Find edges originating from a symbol.
    pub fn find_edges_by_source(&self, source: &SymbolId) -> anyhow::Result<Vec<RawEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT edge_id, source, target, kind, confidence, provenance,
                    ref_id, location_0, location_1, location_2, location_3, location_4, location_5,
                    metadata, resolved_by
             FROM symbol_edges WHERE source = ?1",
        )?;
        let rows = stmt.query_map(params![source], row_to_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find edges targeting a symbol.
    pub fn find_edges_by_target(&self, target: &SymbolId) -> anyhow::Result<Vec<RawEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT edge_id, source, target, kind, confidence, provenance,
                    ref_id, location_0, location_1, location_2, location_3, location_4, location_5,
                    metadata, resolved_by
             FROM symbol_edges WHERE target = ?1",
        )?;
        let rows = stmt.query_map(params![target], row_to_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find all imports for a file.
    pub fn find_imports_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<ImportDef>> {
        let conn = self.lock();
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

    /// Find all scopes for a file.
    pub fn find_scopes_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<ScopeDef>> {
        let conn = self.lock();
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

    /// Find symbols by qualified name (exact match, index lookup).
    pub fn find_symbols_by_qname(&self, qname: &str) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json
             FROM symbols WHERE qualified_name = ?1",
        )?;
        let rows = stmt.query_map(params![qname], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Load ALL symbols (for GraphSnapshot construction).
    /// Uses a separate read connection to avoid blocking writes via the mutex.
    pub fn get_all_symbols(&self) -> anyhow::Result<Vec<SymbolDef>> {
        let guard = self.lock();
        let conn: &Connection = &guard;
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json
             FROM symbols",
        )?;
        let rows = stmt.query_map([], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Alias for `get_all_symbols` (P4: GlobalSymbolIndex construction).
    pub fn load_all_symbols(&self) -> anyhow::Result<Vec<SymbolDef>> {
        self.get_all_symbols()
    }

    /// Load ALL edges (for GraphSnapshot construction).
    /// Uses the shared connection via the mutex; long-running reads may
    //  block writes.  In the future this should use a separate read connection.
    pub fn get_all_edges(&self) -> anyhow::Result<Vec<RawEdge>> {
        let guard = self.lock();
        let conn: &Connection = &guard;
        let mut stmt = conn.prepare(
            "SELECT edge_id, source, target, kind, confidence, provenance,
                    ref_id, location_0, location_1, location_2, location_3, location_4, location_5,
                    metadata, resolved_by FROM symbol_edges",
        )?;
        let rows = stmt.query_map([], row_to_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // ── Binding + Dataflow — write APIs ──
    // -----------------------------------------------------------------------

    /// Batch-insert bindings.
    pub fn insert_bindings(&self, bindings: &[BindingDef]) -> anyhow::Result<()> {
        if bindings.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_bindings(tx, bindings))
    }

    /// Batch-insert binding uses.
    pub fn insert_binding_uses(&self, uses: &[BindingUse]) -> anyhow::Result<()> {
        if uses.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_binding_uses(tx, uses))
    }

    /// Batch-insert data nodes.
    pub fn insert_data_nodes(&self, nodes: &[DataNode]) -> anyhow::Result<()> {
        if nodes.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_data_nodes(tx, nodes))
    }

    /// Batch-insert dataflow edges.
    pub fn insert_dataflow_edges(&self, edges: &[DataFlowEdge]) -> anyhow::Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_dataflow_edges(tx, edges))
    }

    /// Batch-insert CFG nodes.
    pub fn insert_cfg_nodes(&self, nodes: &[CfgNode]) -> anyhow::Result<()> {
        if nodes.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_cfg_nodes(tx, nodes))
    }

    /// Batch-insert CFG edges.
    pub fn insert_cfg_edges(&self, edges: &[CfgEdge]) -> anyhow::Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_cfg_edges(tx, edges))
    }

    // -----------------------------------------------------------------------
    /// Find all callsites in a file.
    pub fn find_callsites_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<Callsite>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT callsite_id, reference_id, caller, callee, receiver, args_json,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    callee_start_line, callee_start_column, callee_end_line, callee_end_column,
                    callee_start_byte, callee_end_byte
             FROM callsites WHERE EXISTS (
                 SELECT 1 FROM symbols WHERE symbols.symbol_id = callsites.caller AND symbols.file_id = ?1
             )",
        )?;
        let rows = stmt.query_map(params![file_id], row_to_callsite)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find all callsites that target a specific callee symbol.
    ///
    /// Used by summary-bridge trace to find callers of a function.
    pub fn find_callsites_by_callee(
        &self,
        callee: &SymbolId,
    ) -> anyhow::Result<Vec<Callsite>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT callsite_id, reference_id, caller, callee, receiver, args_json,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    callee_start_line, callee_start_column, callee_end_line, callee_end_column,
                    callee_start_byte, callee_end_byte
             FROM callsites WHERE callee = ?1",
        )?;
        let rows = stmt.query_map(params![callee], row_to_callsite)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find a single callsite by its ID.
    pub fn find_callsites_by_id(
        &self,
        callsite_id: &CallsiteId,
    ) -> anyhow::Result<Vec<Callsite>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT callsite_id, reference_id, caller, callee, receiver, args_json,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    callee_start_line, callee_start_column, callee_end_line, callee_end_column,
                    callee_start_byte, callee_end_byte
             FROM callsites WHERE callsite_id = ?1",
        )?;
        let rows = stmt.query_map(params![callsite_id], row_to_callsite)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // ── Binding + Dataflow — query APIs ──
    // -----------------------------------------------------------------------

    /// Find bindings for a function.
    pub fn find_bindings_by_function(
        &self,
        function_id: &SymbolId,
    ) -> anyhow::Result<Vec<BindingDef>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT binding_id, file_id, function_id, scope_id, kind, name, symbol_id,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM bindings WHERE function_id = ?1",
        )?;
        let rows = stmt.query_map(params![function_id], row_to_binding)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find all bindings in a file.
    pub fn find_bindings_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<BindingDef>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT binding_id, file_id, function_id, scope_id, kind, name, symbol_id,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM bindings WHERE file_id = ?1",
        )?;
        let rows = stmt.query_map(params![file_id], row_to_binding)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find binding uses for a specific binding.
    pub fn find_binding_uses_by_binding(
        &self,
        binding_id: &BindingId,
    ) -> anyhow::Result<Vec<BindingUse>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT binding_use_id, file_id, scope_id, binding_id, reference_id, name,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM binding_uses WHERE binding_id = ?1",
        )?;
        let rows = stmt.query_map(params![binding_id], row_to_binding_use)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find all binding uses in a file.
    pub fn find_binding_uses_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<BindingUse>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT binding_use_id, file_id, scope_id, binding_id, reference_id, name,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM binding_uses WHERE file_id = ?1",
        )?;
        let rows = stmt.query_map(params![file_id], row_to_binding_use)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find data nodes for a function.
    pub fn find_data_nodes_by_function(
        &self,
        function_id: &SymbolId,
    ) -> anyhow::Result<Vec<DataNode>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT data_node_id, file_id, function_id, kind, binding_id, callsite_id,
                    name, access_path,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM data_nodes WHERE function_id = ?1",
        )?;
        let rows = stmt.query_map(params![function_id], row_to_data_node)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get a single data node by ID.
    pub fn get_data_node(&self, node_id: &DataNodeId) -> anyhow::Result<Option<DataNode>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT data_node_id, file_id, function_id, kind, binding_id, callsite_id,
                    name, access_path,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM data_nodes WHERE data_node_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![node_id], row_to_data_node)?;
        match rows.next() {
            Some(Ok(node)) => Ok(Some(node)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Batch-lookup data nodes by IDs in a single query.
    pub(crate) fn get_data_nodes(
        &self,
        ids: &[DataNodeId],
    ) -> anyhow::Result<HashMap<DataNodeId, DataNode>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.lock();
        let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "SELECT data_node_id, file_id, function_id, kind, binding_id, callsite_id,
                    name, access_path,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM data_nodes WHERE data_node_id IN ({})",
            placeholders.join(","),
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> =
            ids.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
        let rows = stmt.query_map(params.as_slice(), |row| {
            let node: DataNode = row_to_data_node(row)?;
            Ok((node.id, node))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (id, node) = row?;
            map.insert(id, node);
        }
        Ok(map)
    }

    /// Find all data nodes in a file.
    pub fn find_data_nodes_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<DataNode>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT data_node_id, file_id, function_id, kind, binding_id, callsite_id,
                    name, access_path,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM data_nodes WHERE file_id = ?1",
        )?;
        let rows = stmt.query_map(params![file_id], row_to_data_node)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find data nodes associated with a specific callsite (e.g. CallArg nodes).
    ///
    /// Used by summary-bridge trace to find call-arg data nodes for a given callsite.
    pub fn find_data_nodes_by_callsite(
        &self,
        callsite_id: &CallsiteId,
    ) -> anyhow::Result<Vec<DataNode>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT data_node_id, file_id, function_id, kind, binding_id, callsite_id,
                    name, access_path,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM data_nodes WHERE callsite_id = ?1",
        )?;
        let rows = stmt.query_map(params![callsite_id], row_to_data_node)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find dataflow edges originating from a data node.
    pub fn find_dataflow_edges_by_source(
        &self,
        source: &DataNodeId,
    ) -> anyhow::Result<Vec<DataFlowEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT dataflow_edge_id, source, target, kind,
                    location_0, location_1, location_2,
                    location_3, location_4, location_5, confidence
             FROM dataflow_edges WHERE source = ?1",
        )?;
        let rows = stmt.query_map(params![source], row_to_dataflow_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find dataflow edges targeting a data node.
    pub fn find_dataflow_edges_by_target(
        &self,
        target: &DataNodeId,
    ) -> anyhow::Result<Vec<DataFlowEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT dataflow_edge_id, source, target, kind,
                    location_0, location_1, location_2,
                    location_3, location_4, location_5, confidence
             FROM dataflow_edges WHERE target = ?1",
        )?;
        let rows = stmt.query_map(params![target], row_to_dataflow_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Batch-lookup dataflow edges by source IDs in a single query.
    pub(crate) fn find_dataflow_edges_by_sources(
        &self,
        sources: &[DataNodeId],
    ) -> anyhow::Result<Vec<DataFlowEdge>> {
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let placeholders: Vec<String> = (0..sources.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "SELECT dataflow_edge_id, source, target, kind,
                    location_0, location_1, location_2,
                    location_3, location_4, location_5, confidence
             FROM dataflow_edges WHERE source IN ({})",
            placeholders.join(","),
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> =
            sources.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
        let rows = stmt.query_map(params.as_slice(), row_to_dataflow_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find a callsite by its originating reference ID.
    pub fn find_callsite_by_reference_id(
        &self,
        ref_id: &ReferenceId,
    ) -> anyhow::Result<Option<Callsite>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT callsite_id, reference_id, caller, callee, receiver, args_json,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    callee_start_line, callee_start_column, callee_end_line, callee_end_column,
                    callee_start_byte, callee_end_byte
             FROM callsites WHERE reference_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![ref_id], row_to_callsite)?;
        match rows.next() {
            Some(Ok(cs)) => Ok(Some(cs)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Update the `callee` field of the callsite linked to a given reference.
    ///
    /// Called by GraphBuilder when a Calls/Instantiates edge is resolved,
    /// linking the callsite to the resolved target symbol.
    pub fn update_callsite_callee(
        &self,
        ref_id: &ReferenceId,
        callee: &SymbolId,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE callsites SET callee = ?1 WHERE reference_id = ?2",
            params![callee, ref_id],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // ── CFG — query APIs ──
    // -----------------------------------------------------------------------

    /// Find all CFG nodes for a function.
    pub fn find_cfg_nodes_by_function(
        &self,
        function_id: &SymbolId,
    ) -> anyhow::Result<Vec<CfgNode>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT cfg_node_id, function_id, kind,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM cfg_nodes WHERE function_id = ?1",
        )?;
        let rows = stmt.query_map(params![function_id], row_to_cfg_node)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find CFG edges originating from a CFG node.
    pub fn find_cfg_edges_by_source(&self, source: &CfgNodeId) -> anyhow::Result<Vec<CfgEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT cfg_edge_id, source_node, target_node, kind FROM cfg_edges WHERE source_node = ?1",
        )?;
        let rows = stmt.query_map(params![source], row_to_cfg_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // FileFacts — convenience batch insert
    // -----------------------------------------------------------------------

    /// Insert all components of a `FileFacts` in a single transaction.
    /// This is the primary write path from extraction.
    pub fn insert_file_facts(&self, facts: &FileFacts) -> anyhow::Result<()> {
        self.insert_file_facts_impl(std::slice::from_ref(facts))
    }

    /// Batch-insert multiple `FileFacts` in a single transaction (P3: bulk write).
    ///
    /// This avoids per-file transaction overhead. All files are committed
    /// atomically. Use this for fresh/rebuild indexes; incremental sync may
    /// prefer the single-file path for finer-grained failure isolation.
    pub fn insert_file_facts_batch(&self, batch: &[FileFacts]) -> anyhow::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        self.insert_file_facts_impl(batch)
    }

    /// Shared implementation: one transaction, one lock, N files.
    fn insert_file_facts_impl(&self, batch: &[FileFacts]) -> anyhow::Result<()> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction()?;

        for facts in batch {
            // File info
            tx.execute(
                r#"INSERT OR REPLACE INTO files
                   (file_id, path, language, content_hash, status, index_time)
                   VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))"#,
                params![
                    facts.file.file_id,
                    facts.file.path,
                    facts.file.language.as_str(),
                    facts.file.content_hash,
                    facts.file.status.as_str(),
                ],
            )?;

            if !facts.symbols.is_empty() {
                write_symbols(&tx, &facts.symbols)?;
            }
            if !facts.scopes.is_empty() {
                write_scopes(&tx, &facts.scopes)?;
            }
            if !facts.references.is_empty() {
                write_references(&tx, &facts.references)?;
            }
            if !facts.imports.is_empty() {
                write_imports(&tx, &facts.imports)?;
            }
            // Defensive FK guard
            let valid_sources: HashSet<_> = facts.symbols.iter().map(|s| s.id).collect();
            if !facts.raw_edges.is_empty() {
                let valid_edges: Vec<_> = facts
                    .raw_edges
                    .iter()
                    .filter(|edge| valid_sources.contains(&edge.source))
                    .cloned()
                    .collect();
                if !valid_edges.is_empty() {
                    write_edges(&tx, &valid_edges)?;
                }
            }
            if !facts.callsites.is_empty() {
                let valid_callsites: Vec<_> = facts
                    .callsites
                    .iter()
                    .filter(|callsite| valid_sources.contains(&callsite.caller))
                    .cloned()
                    .collect();
                if !valid_callsites.is_empty() {
                    write_callsites(&tx, &valid_callsites)?;
                }
            }

            // Binding + Dataflow data
            if !facts.bindings.is_empty() {
                write_bindings(&tx, &facts.bindings)?;
            }
            if !facts.binding_uses.is_empty() {
                write_binding_uses(&tx, &facts.binding_uses)?;
            }
            if !facts.data_nodes.is_empty() {
                write_data_nodes(&tx, &facts.data_nodes)?;
            }
            if !facts.dataflow_edges.is_empty() {
                write_dataflow_edges(&tx, &facts.dataflow_edges)?;
            }
            if !facts.cfg_nodes.is_empty() {
                write_cfg_nodes(&tx, &facts.cfg_nodes)?;
            }
            if !facts.cfg_edges.is_empty() {
                write_cfg_edges(&tx, &facts.cfg_edges)?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Project metadata (key-value)
    // -----------------------------------------------------------------------

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
        conn.execute(
            "DELETE FROM project_metadata WHERE key = ?1",
            params![key],
        )?;
        Ok(())
    }

    /// Get a project metadata value by key.
    pub fn get_metadata(&self, key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT value FROM project_metadata WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Get the schema version from the database.
    pub fn schema_version(&self) -> anyhow::Result<i64> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT MAX(version) FROM schema_versions")?;
        let version: Option<i64> = stmt.query_row([], |row| row.get(0))?;
        Ok(version.unwrap_or(0))
    }

    // -----------------------------------------------------------------------
    // Stats
    // -----------------------------------------------------------------------

    /// Collection metrics about the indexed codebase.
    pub fn get_stats(&self) -> anyhow::Result<StoreStats> {
        let conn = self.lock();
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
            .filter_map(|r| r.ok())
            .collect();

        // Files grouped by language
        let mut stmt = conn.prepare(
            "SELECT language, COUNT(*) FROM files GROUP BY language ORDER BY COUNT(*) DESC",
        )?;
        let files_by_language: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
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

    // ── Optimized indexed queries ─────────────────────────────────────────

    /// Find symbols by exact `name` match (uses index on `symbols.name`).
    ///
    /// Faster than `search_symbols_by_name_like` for exact-match lookups,
    /// avoiding FTS5 overhead and LIKE scans.
    pub fn find_symbols_by_name(&self, name: &str) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json
             FROM symbols WHERE name = ?1 ORDER BY qualified_name",
        )?;
        let rows = stmt.query_map(params![name], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Resolve a user-facing path to a [`FileId`] using indexed `files.path`.
    ///
    /// Tries exact match on `path = ?`, then suffix match on
    /// `path LIKE '%/' || ?`. Falls back to Rust path normalization for the
    /// suffix case. This replaces the old `list_files()` → linear scan pattern.
    pub fn resolve_file_id(
        &self,
        root: &Path,
        rel_path: &str,
    ) -> anyhow::Result<Option<FileId>> {
        let conn = self.lock();

        // 1. Exact path match.
        let mut stmt = conn.prepare("SELECT file_id FROM files WHERE path = ?1")?;
        if let Some(row) = stmt.query(params![rel_path])?.next()? {
            return Ok(Some(row.get(0)?));
        }

        // 2. Suffix match (e.g. "helper.ts" matches "src/lib/helper.ts").
        let pattern = format!("%/{}", rel_path);
        let mut stmt = conn.prepare(
            "SELECT file_id, path FROM files WHERE path LIKE ?1 LIMIT 5",
        )?;
        let rows: Vec<_> = stmt
            .query_map(params![&pattern], |row| {
                Ok((row.get::<_, FileId>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
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

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

/// Indexed codebase metrics.
#[derive(Debug, Clone)]
pub struct StoreStats {
    pub total_files: i64,
    pub total_symbols: i64,
    pub total_edges: i64,
    pub total_references: i64,
    pub unresolved_references: i64,
    pub sqlite_version: String,
    /// Symbol counts grouped by kind (e.g. {"class": 42, "function": 128}).
    pub symbols_by_kind: Vec<(String, i64)>,
    /// File counts grouped by language (e.g. {"typescript": 50, "python": 12}).
    pub files_by_language: Vec<(String, i64)>,
}

// ── Reader trait implementations ────────────────────────────────────────────
//
// The 4 reader traits (SymbolReader, DataflowReader, CallGraphReader,
// FileReader) are defined in [crate::db::readers] and implemented on Store
// below.  Each delegates via UFCS to the store's inherent method.
//
// trace/analysis code can accept `impl SymbolReader + DataflowReader +
// CallGraphReader + FileReader` instead of `&Store` for layered access.

use super::readers::*;

impl SymbolReader for Store {
    fn find_symbol_by_id(&self, id: &SymbolId) -> anyhow::Result<Option<SymbolDef>> {
        StoreReader::find_symbol_by_id(self.deref(), id)
    }
    fn find_symbols_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<SymbolDef>> {
        Store::find_symbols_by_file(self, file_id)
    }
    fn search_symbols(&self, query: &str) -> anyhow::Result<Vec<SymbolDef>> {
        Store::search_symbols(self, query)
    }
    fn search_symbols_with_limit(&self, query: &str, limit: usize, kind_filter: Option<&SymbolKind>) -> anyhow::Result<Vec<SymbolDef>> {
        Store::search_symbols_with_limit(self, query, limit, kind_filter)
    }
    fn search_symbols_by_name_like(&self, pattern: &str, language: Option<&Language>, limit: usize, kind_filter: Option<&SymbolKind>) -> anyhow::Result<Vec<SymbolDef>> {
        Store::search_symbols_by_name_like(self, pattern, language, limit, kind_filter)
    }
    fn count_symbols(&self) -> anyhow::Result<usize> {
        Store::count_symbols(self)
    }
    fn find_symbols_by_qname(&self, qname: &str) -> anyhow::Result<Vec<SymbolDef>> {
        Store::find_symbols_by_qname(self, qname)
    }
    fn get_all_symbols(&self) -> anyhow::Result<Vec<SymbolDef>> {
        Store::get_all_symbols(self)
    }
    fn find_symbols_by_name(&self, name: &str) -> anyhow::Result<Vec<SymbolDef>> {
        Store::find_symbols_by_name(self, name)
    }
    fn find_references_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<ReferenceUse>> {
        Store::find_references_by_file(self, file_id)
    }
    fn find_scopes_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<ScopeDef>> {
        Store::find_scopes_by_file(self, file_id)
    }
    fn find_imports_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<ImportDef>> {
        Store::find_imports_by_file(self, file_id)
    }
    fn find_edges_by_source(&self, source: &SymbolId) -> anyhow::Result<Vec<RawEdge>> {
        Store::find_edges_by_source(self, source)
    }
    fn find_edges_by_target(&self, target: &SymbolId) -> anyhow::Result<Vec<RawEdge>> {
        Store::find_edges_by_target(self, target)
    }
    fn get_all_edges(&self) -> anyhow::Result<Vec<RawEdge>> {
        Store::get_all_edges(self)
    }
}

impl DataflowReader for Store {
    fn get_data_node(&self, id: &DataNodeId) -> anyhow::Result<Option<DataNode>> {
        Store::get_data_node(self, id)
    }
    fn find_data_nodes_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<DataNode>> {
        Store::find_data_nodes_by_file(self, file_id)
    }
    fn find_data_nodes_by_function(&self, function_id: &SymbolId) -> anyhow::Result<Vec<DataNode>> {
        Store::find_data_nodes_by_function(self, function_id)
    }
    fn find_data_nodes_by_callsite(&self, callsite_id: &CallsiteId) -> anyhow::Result<Vec<DataNode>> {
        Store::find_data_nodes_by_callsite(self, callsite_id)
    }
    fn find_dataflow_edges_by_source(&self, source: &DataNodeId) -> anyhow::Result<Vec<DataFlowEdge>> {
        Store::find_dataflow_edges_by_source(self, source)
    }
    fn find_dataflow_edges_by_target(&self, target: &DataNodeId) -> anyhow::Result<Vec<DataFlowEdge>> {
        Store::find_dataflow_edges_by_target(self, target)
    }
}

impl CallGraphReader for Store {
    fn find_callsites_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<Callsite>> {
        Store::find_callsites_by_file(self, file_id)
    }
    fn find_callsites_by_callee(&self, callee: &SymbolId) -> anyhow::Result<Vec<Callsite>> {
        Store::find_callsites_by_callee(self, callee)
    }
    fn find_callsites_by_id(&self, id: &CallsiteId) -> anyhow::Result<Vec<Callsite>> {
        Store::find_callsites_by_id(self, id)
    }
    fn find_callsite_by_reference_id(&self, reference_id: &ReferenceId) -> anyhow::Result<Option<Callsite>> {
        Store::find_callsite_by_reference_id(self, reference_id)
    }
    fn find_bindings_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<BindingDef>> {
        Store::find_bindings_by_file(self, file_id)
    }
    fn find_bindings_by_function(&self, function_id: &SymbolId) -> anyhow::Result<Vec<BindingDef>> {
        Store::find_bindings_by_function(self, function_id)
    }
    fn find_binding_uses_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<BindingUse>> {
        Store::find_binding_uses_by_file(self, file_id)
    }
    fn find_binding_uses_by_binding(&self, binding_id: &BindingId) -> anyhow::Result<Vec<BindingUse>> {
        Store::find_binding_uses_by_binding(self, binding_id)
    }
    fn find_cfg_nodes_by_function(&self, function_id: &SymbolId) -> anyhow::Result<Vec<CfgNode>> {
        Store::find_cfg_nodes_by_function(self, function_id)
    }
    fn find_cfg_edges_by_source(&self, source: &CfgNodeId) -> anyhow::Result<Vec<CfgEdge>> {
        Store::find_cfg_edges_by_source(self, source)
    }
}

impl FileReader for Store {
    fn get_file(&self, file_id: &FileId) -> anyhow::Result<Option<FileInfo>> {
        Store::get_file(self, file_id)
    }
    fn list_files(&self) -> anyhow::Result<Vec<FileInfo>> {
        Store::list_files(self)
    }
    fn resolve_file_id(&self, root: &Path, rel_path: &str) -> anyhow::Result<Option<FileId>> {
        Store::resolve_file_id(self, root, rel_path)
    }
    fn get_metadata(&self, key: &str) -> anyhow::Result<Option<String>> {
        Store::get_metadata(self, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        store
    }

    fn test_file() -> FileInfo {
        FileInfo {
            file_id: FileId::generate("src/test.ts"),
            path: "src/test.ts".into(),
            language: Language::TypeScript,
            content_hash: "abc123".into(),
            status: ParseStatus::Success,
        }
    }

    fn test_symbol(file_id: FileId, name: &str, kind: SymbolKind) -> SymbolDef {
        let range = TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 11,
        };
        let id = SymbolId::generate(&file_id, "typescript", name, kind.as_str(), None);
        SymbolDef {
            id,
            kind,
            name: name.to_string(),
            qualified_name: format!("{}.{}", name, name),
            symbol_path: vec![name.to_string()],
            file_id,
            language: Language::TypeScript,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
        }
    }

    #[test]
    fn test_store_open_in_memory() {
        let store = test_store();
        let stats = store.get_stats().unwrap();
        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.total_symbols, 0);
    }

    #[test]
    fn test_upsert_and_get_file() {
        let store = test_store();
        let file = test_file();
        store.upsert_file(&file).unwrap();
        let got = store.get_file(&file.file_id).unwrap().unwrap();
        assert_eq!(got.path, "src/test.ts");
        assert_eq!(got.language, Language::TypeScript);
    }

    #[test]
    fn test_insert_and_find_symbol() {
        let store = test_store();
        let file = test_file();
        store.upsert_file(&file).unwrap();

        let sym = test_symbol(file.file_id, "MyClass", SymbolKind::Class);
        store.insert_symbols(&[sym.clone()]).unwrap();

        let found = store.find_symbol_by_id(&sym.id).unwrap().unwrap();
        assert_eq!(found.name, "MyClass");
        assert_eq!(found.kind, SymbolKind::Class);
    }

    #[test]
    fn test_insert_references_and_find_unresolved() {
        let store = test_store();
        let file = test_file();
        store.upsert_file(&file).unwrap();

        let sym = test_symbol(file.file_id, "target", SymbolKind::Function);
        store.insert_symbols(&[sym.clone()]).unwrap();

        let range = TextRange {
            start_byte: 50,
            end_byte: 56,
            start_line: 3,
            start_column: 5,
            end_line: 3,
            end_column: 11,
        };
        let ref_id = ReferenceId::generate(
            &file.file_id,
            Some(&sym.id),
            range.start_byte,
            range.end_byte,
            "target",
            ReferenceKind::Call,
        );
        let r = ReferenceUse {
            id: ref_id.clone(),
            file_id: file.file_id,
            source_symbol: Some(sym.id),
            scope_id: None,
            kind: ReferenceKind::Call,
            text: "target".into(),
            name: "target".into(),
            receiver: None,
            arity: Some(1),
            range,
            binding_id: None,
            resolved: None,
        };
        store.insert_references(&[r]).unwrap();

        let unresolved = store.find_unresolved_references().unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].id, ref_id);
    }

    #[test]
    fn test_update_reference_resolution() {
        let store = test_store();
        let file = test_file();
        store.upsert_file(&file).unwrap();

        let src = test_symbol(file.file_id, "caller", SymbolKind::Function);
        let tgt = test_symbol(file.file_id, "callee", SymbolKind::Function);
        store.insert_symbols(&[src.clone(), tgt.clone()]).unwrap();

        let range = TextRange {
            start_byte: 100,
            end_byte: 106,
            start_line: 5,
            start_column: 3,
            end_line: 5,
            end_column: 9,
        };
        let ref_id = ReferenceId::generate(
            &file.file_id,
            Some(&src.id),
            range.start_byte,
            range.end_byte,
            "callee",
            ReferenceKind::Call,
        );
        let r = ReferenceUse {
            id: ref_id.clone(),
            file_id: file.file_id,
            source_symbol: Some(src.id),
            scope_id: None,
            kind: ReferenceKind::Call,
            text: "callee".into(),
            name: "callee".into(),
            receiver: None,
            arity: None,
            range,
            binding_id: None,
            resolved: None,
        };
        store.insert_references(&[r]).unwrap();

        let target = ResolvedTarget {
            symbol_id: tgt.id,
            confidence: Confidence::certain(),
            strategy: ResolutionStrategy::ExactMatch,
            provenance: Provenance::TreeSitter,
        };
        store.update_reference_resolution(&ref_id, &target).unwrap();

        let unresolved = store.find_unresolved_references().unwrap();
        assert!(unresolved.is_empty());
    }

    #[test]
    fn test_insert_file_facts() {
        let store = test_store();
        let file_id = FileId::generate("src/example.py");

        let sym = test_symbol(file_id, "hello", SymbolKind::Function);
        let facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "src/example.py".into(),
                language: Language::Python,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            },
            symbols: vec![sym],
            ..Default::default()
        };

        store.insert_file_facts(&facts).unwrap();

        let stats = store.get_stats().unwrap();
        assert_eq!(stats.total_files, 1);
        assert_eq!(stats.total_symbols, 1);
    }

    #[test]
    fn test_delete_file_cascades() {
        let store = test_store();
        let file_id = FileId::generate("src/temp.ts");
        let sym = test_symbol(file_id, "Temp", SymbolKind::Class);

        let facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "src/temp.ts".into(),
                language: Language::TypeScript,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            },
            symbols: vec![sym],
            ..Default::default()
        };
        store.insert_file_facts(&facts).unwrap();
        assert_eq!(store.get_stats().unwrap().total_symbols, 1);

        store.delete_file_data(&file_id).unwrap();
        assert_eq!(store.get_stats().unwrap().total_symbols, 0);
    }
}
