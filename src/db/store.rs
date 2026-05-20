//! Atlas Store — the single SQLite persistence layer.
//!
//! `Store` wraps a `Mutex<Connection>` and provides all CRUD operations.
//! For MVP, a single writer/reader suffices. Future: split into `StoreWriter`
//! and `StoreReader` for concurrent read access.

use crate::db::schema::{CURRENT_SCHEMA_VERSION, SCHEMA_DDL};

use crate::types::*;
use rusqlite::{params, Connection, Transaction};
use std::collections::HashSet;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
        self.conn.lock().unwrap()
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
}

// ---------------------------------------------------------------------------
// Store — read/write persistence layer
// ---------------------------------------------------------------------------

/// Thread-safe SQLite persistence layer.
///
/// Derefs to `StoreReader` for all read operations. Write operations
/// are defined directly on `Store`.
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
        self.reader.conn.lock().unwrap()
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

        // Record current version if not already present
        conn.execute(
            "INSERT OR IGNORE INTO schema_versions (version, description)
             VALUES (?1, ?2)",
            params![CURRENT_SCHEMA_VERSION, "Atlas-native schema v5: P3 bindings/dataflow tables, edges→symbol_edges"],
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
                        Ok(Some((parts[0].parse().unwrap_or(0), parts[1].parse().unwrap_or(0))))
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
        if like_pattern.len() <= 2 {  // Just "%%"
            return Ok(Vec::new());
        }

        // Build WHERE clause dynamically based on filters
        let mut where_clauses = vec![
            "(s.name LIKE ?1 OR s.qualified_name LIKE ?2)".to_string(),
        ];
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
        let kind_str = kind_filter.map(|k| k.as_str().to_string()).unwrap_or_default();

        let rows = match (language.is_some(), kind_filter.is_some()) {
            (true, true) => {
                stmt.query_map(params![like_pattern, like_pattern, lang_str, kind_str], row_to_symbol)?
            }
            (true, false) => {
                stmt.query_map(params![like_pattern, like_pattern, lang_str], row_to_symbol)?
            }
            (false, true) => {
                stmt.query_map(params![like_pattern, like_pattern, kind_str], row_to_symbol)?
            }
            (false, false) => {
                stmt.query_map(params![like_pattern, like_pattern], row_to_symbol)?
            }
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
        let mut stmt = conn.prepare(
            &format!(
                "{} WHERE resolved_symbol_id IS NULL",
                REFERENCE_SELECT_NO_WHERE
            ),
        )?;
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

    /// Load ALL edges (for GraphSnapshot construction).
    /// Uses a separate read connection to avoid blocking writes via the mutex.
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
    // P3: Binding + Dataflow — write APIs
    // -----------------------------------------------------------------------

    /// Batch-insert bindings.
    pub fn insert_bindings(&self, bindings: &[BindingDef]) -> anyhow::Result<()> {
        if bindings.is_empty() { return Ok(()); }
        self.with_transaction(|tx| write_bindings(tx, bindings))
    }

    /// Batch-insert binding uses.
    pub fn insert_binding_uses(&self, uses: &[BindingUse]) -> anyhow::Result<()> {
        if uses.is_empty() { return Ok(()); }
        self.with_transaction(|tx| write_binding_uses(tx, uses))
    }

    /// Batch-insert data nodes.
    pub fn insert_data_nodes(&self, nodes: &[DataNode]) -> anyhow::Result<()> {
        if nodes.is_empty() { return Ok(()); }
        self.with_transaction(|tx| write_data_nodes(tx, nodes))
    }

    /// Batch-insert dataflow edges.
    pub fn insert_dataflow_edges(&self, edges: &[DataFlowEdge]) -> anyhow::Result<()> {
        if edges.is_empty() { return Ok(()); }
        self.with_transaction(|tx| write_dataflow_edges(tx, edges))
    }

    /// Batch-insert callsite args.
    pub fn insert_callsite_args(&self, args: &[CallsiteArg]) -> anyhow::Result<()> {
        if args.is_empty() { return Ok(()); }
        self.with_transaction(|tx| write_callsite_args(tx, args))
    }

    // -----------------------------------------------------------------------
    // P3: Binding + Dataflow — query APIs
    // -----------------------------------------------------------------------

    /// Find bindings for a function.
    pub fn find_bindings_by_function(&self, function_id: &SymbolId) -> anyhow::Result<Vec<BindingDef>> {
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

    /// Find binding uses for a specific binding.
    pub fn find_binding_uses_by_binding(&self, binding_id: &BindingId) -> anyhow::Result<Vec<BindingUse>> {
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

    /// Find data nodes for a function.
    pub fn find_data_nodes_by_function(&self, function_id: &SymbolId) -> anyhow::Result<Vec<DataNode>> {
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

    /// Find dataflow edges originating from a data node.
    pub fn find_dataflow_edges_by_source(&self, source: &DataNodeId) -> anyhow::Result<Vec<DataFlowEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT dataflow_edge_id, source, target, kind, location_0, location_1, location_2, confidence
             FROM dataflow_edges WHERE source = ?1",
        )?;
        let rows = stmt.query_map(params![source], row_to_dataflow_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find dataflow edges targeting a data node.
    pub fn find_dataflow_edges_by_target(&self, target: &DataNodeId) -> anyhow::Result<Vec<DataFlowEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT dataflow_edge_id, source, target, kind, location_0, location_1, location_2, confidence
             FROM dataflow_edges WHERE target = ?1",
        )?;
        let rows = stmt.query_map(params![target], row_to_dataflow_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // FileFacts — convenience batch insert
    // -----------------------------------------------------------------------

    /// Insert all components of a `FileFacts` in a single transaction.
    /// This is the primary write path from extraction.
    pub fn insert_file_facts(&self, facts: &FileFacts) -> anyhow::Result<()> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction()?;

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
        // Defensive FK guard: extraction should already resolve all source
        // ownership through SymbolRegistry, but the store is the last line of
        // defense against a single ghost edge/callsite rolling back an entire
        // file's valid facts.
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

        // P3: Binding + Dataflow data
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
        if !facts.callsite_args.is_empty() {
            write_callsite_args(&tx, &facts.callsite_args)?;
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
        let mut stmt = conn.prepare(
            "SELECT MAX(version) FROM schema_versions"
        )?;
        let version: Option<i64> = stmt.query_row([], |row| row.get(0))?;
        Ok(version.unwrap_or(0))
    }

    // -----------------------------------------------------------------------
    // Stats
    // -----------------------------------------------------------------------

    /// Collection metrics about the indexed codebase.
    pub fn get_stats(&self) -> anyhow::Result<StoreStats> {
        let conn = self.lock();
        let total_files: i64 =
            conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
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
        let sqlite_version: String =
            conn.query_row("SELECT sqlite_version()", [], |r| r.get(0))?;

        // Symbols grouped by kind
        let mut stmt = conn.prepare(
            "SELECT kind, COUNT(*) FROM symbols GROUP BY kind ORDER BY COUNT(*) DESC",
        )?;
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

// ---------------------------------------------------------------------------
// Row mapping helpers
// ---------------------------------------------------------------------------

const REFERENCE_SELECT_NO_WHERE: &str = r#"
    SELECT reference_id, file_id, source_symbol, scope_id, kind,
           text, name, receiver, arity,
           range_start_byte, range_end_byte, range_start_line,
           range_start_column, range_end_line, range_end_column,
           resolved_symbol_id, resolved_confidence, resolved_strategy,
           resolved_provenance, binding_id
    FROM "references""#;

const REFERENCE_SELECT_WHERE: &str = r#"
    SELECT reference_id, file_id, source_symbol, scope_id, kind,
           text, name, receiver, arity,
           range_start_byte, range_end_byte, range_start_line,
           range_start_column, range_end_line, range_end_column,
           resolved_symbol_id, resolved_confidence, resolved_strategy,
           resolved_provenance, binding_id
    FROM "references" WHERE file_id = ?1"#;

fn row_to_file_info(row: &rusqlite::Row) -> rusqlite::Result<FileInfo> {
    Ok(FileInfo {
        file_id: row.get(0)?,
        path: row.get(1)?,
        language: Language::from_str(row.get::<_, String>(2)?.as_str())
            .unwrap_or_default(),
        content_hash: row.get(3)?,
        status: ParseStatus::from_str(row.get::<_, String>(4)?.as_str())
            .unwrap_or_default(),
    })
}

fn row_to_symbol(row: &rusqlite::Row) -> rusqlite::Result<SymbolDef> {
    let symbol_path_json: String = row.get(5)?;
    let ns_json: String = row.get(27)?;
    Ok(SymbolDef {
        id: row.get(0)?,
        file_id: row.get(1)?,
        kind: SymbolKind::from_str(row.get::<_, String>(2)?.as_str()).unwrap_or(SymbolKind::File),
        name: row.get(3)?,
        qualified_name: row.get(4)?,
        symbol_path: serde_json::from_str(&symbol_path_json).unwrap_or_default(),
        language: Language::from_str(row.get::<_, String>(6)?.as_str()).unwrap_or_default(),
        range: TextRange {
            start_byte: row.get(7)?,
            end_byte: row.get(8)?,
            start_line: row.get(9)?,
            start_column: row.get(10)?,
            end_line: row.get(11)?,
            end_column: row.get(12)?,
        },
        name_range: TextRange {
            start_byte: row.get(13)?,
            end_byte: row.get(14)?,
            start_line: row.get(15)?,
            start_column: row.get(16)?,
            end_line: row.get(17)?,
            end_column: row.get(18)?,
        },
        signature: row.get(19)?,
        visibility: row
            .get::<_, Option<String>>(20)?
            .and_then(|v| Visibility::from_str(&v)),
        exported: row.get::<_, i32>(21)? != 0,
        static_: row.get::<_, i32>(22)? != 0,
        async_: row.get::<_, i32>(23)? != 0,
        container: row.get(24)?,
        scope_id: row.get(25)?,
        package_name: row.get(26)?,
        namespace_path: serde_json::from_str(&ns_json).unwrap_or_default(),
    })
}

fn row_to_reference(row: &rusqlite::Row) -> rusqlite::Result<ReferenceUse> {
    // Gather resolved-target fields outside any map closure so `?` is valid.
    let resolved = {
        let sym: Option<SymbolId> = row.get(15)?;
        match sym {
            Some(sid) => {
                let conf: Option<f32> = row.get(16)?;
                let strat_s: Option<String> = row.get(17)?;
                let prov_s: Option<String> = row.get(18)?;
                Some(ResolvedTarget {
                    symbol_id: sid,
                    confidence: Confidence::new(conf.unwrap_or(0.5)),
                    strategy: ResolutionStrategy::from_str(strat_s.as_deref().unwrap_or(""))
                        .unwrap_or(ResolutionStrategy::ExactMatch),
                    provenance: Provenance::from_str(prov_s.as_deref().unwrap_or(""))
                        .unwrap_or_default(),
                })
            }
            None => None,
        }
    };
    Ok(ReferenceUse {
        id: row.get(0)?,
        file_id: row.get(1)?,
        source_symbol: row.get(2)?,
        scope_id: row.get(3)?,
        kind: ReferenceKind::from_str(row.get::<_, String>(4)?.as_str())
            .unwrap_or(ReferenceKind::Usage),
        text: row.get(5)?,
        name: row.get(6)?,
        receiver: row.get(7)?,
        arity: row.get(8)?,
        range: TextRange {
            start_byte: row.get(9)?,
            end_byte: row.get(10)?,
            start_line: row.get(11)?,
            start_column: row.get(12)?,
            end_line: row.get(13)?,
            end_column: row.get(14)?,
        },
        resolved,
        binding_id: row.get(19)?,
    })
}

fn row_to_edge(row: &rusqlite::Row) -> rusqlite::Result<RawEdge> {
    let ref_id: Option<ReferenceId> = row.get(6)?;
    let location: Option<TextRange> = {
        let sb: Option<u32> = row.get(7)?;
        sb.map(|start_byte| TextRange {
            start_byte,
            end_byte: row.get::<_, u32>(8).unwrap_or(0),
            start_line: row.get::<_, u32>(9).unwrap_or(0),
            start_column: row.get::<_, u32>(10).unwrap_or(0),
            end_line: row.get::<_, u32>(11).unwrap_or(0),
            end_column: row.get::<_, u32>(12).unwrap_or(0),
        })
    };
    let metadata: Option<String> = row.get(13)?;
    let resolved_by_str: Option<String> = row.get(14)?;
    let resolved_by = resolved_by_str
        .as_deref()
        .and_then(|s| ResolutionStrategy::from_str(s));

    Ok(RawEdge {
        id: row.get(0)?,
        source: row.get(1)?,
        target: row.get(2)?,
        kind: EdgeKind::from_str(row.get::<_, String>(3)?.as_str()).unwrap_or(EdgeKind::References),
        confidence: Confidence::new(row.get(4)?),
        provenance: Provenance::from_str(row.get::<_, String>(5)?.as_str())
            .unwrap_or_default(),
        ref_id,
        location,
        metadata,
        resolved_by,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Strip FTS5 special characters to prevent syntax errors.
fn sanitize_fts5_query(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '_' || *c == '.' || *c == '-')
        .collect();
    if sanitized.is_empty() {
        "*".to_string()
    } else {
        sanitized
    }
}

// ---------------------------------------------------------------------------
// Helpers for exclusive lock
// ---------------------------------------------------------------------------

/// Current time in milliseconds since Unix epoch.
fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Check whether a process with the given PID is still alive.
///
/// Uses `kill -0` on Unix (no signal sent, just checks existence).
/// On non-Unix, assumes alive (conservative — won't steal locks).
fn is_process_alive(pid: i64) -> bool {
    #[cfg(unix)]
    {
        // `kill -0 <pid>` checks process existence without sending a signal.
        // This uses the system `kill` command — no external crate needed.
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(true) // conservative: assume alive if check fails
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true // conservative: assume alive
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Private write helpers (take `&Connection` to enable single-transaction bulk writes)
// ---------------------------------------------------------------------------

fn write_symbols(conn: &Connection, symbols: &[SymbolDef]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO symbols
           (symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
            language,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column,
            name_start_byte, name_end_byte, name_start_line, name_start_column,
            name_end_line, name_end_column,
            signature, visibility, exported, static_, async_,
            container_id, scope_id, package_name, namespace_path_json)
        VALUES (
            ?1,?2,?3,?4,?5,?6,?7,
            ?8,?9,?10,?11,?12,?13,
            ?14,?15,?16,?17,?18,?19,
            ?20,?21,?22,?23,?24,
            ?25,?26,?27,?28
        )"#,
    )?;
    for s in symbols {
        let path_json = serde_json::to_string(&s.symbol_path)?;
        let ns_json = serde_json::to_string(&s.namespace_path)?;
        stmt.execute(params![
            s.id, s.file_id, s.kind.as_str(), s.name, s.qualified_name, path_json,
            s.language.as_str(),
            s.range.start_byte, s.range.end_byte, s.range.start_line, s.range.start_column,
            s.range.end_line, s.range.end_column,
            s.name_range.start_byte, s.name_range.end_byte, s.name_range.start_line,
            s.name_range.start_column, s.name_range.end_line, s.name_range.end_column,
            s.signature, s.visibility.map(|v| v.as_str()),
            s.exported as i32, s.static_ as i32, s.async_ as i32,
            s.container, s.scope_id, s.package_name, ns_json,
        ])?;
    }
    Ok(())
}

fn write_scopes(conn: &Connection, scopes: &[ScopeDef]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO scopes
            (scope_id, file_id, kind, name, scope_path, parent_id,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"#,
    )?;
    for sc in scopes {
        stmt.execute(params![
            sc.id, sc.file_id, sc.kind.as_str(), sc.name, sc.scope_path, sc.parent_id,
            sc.range.start_byte, sc.range.end_byte, sc.range.start_line,
            sc.range.start_column, sc.range.end_line, sc.range.end_column,
        ])?;
    }
    Ok(())
}

fn write_references(conn: &Connection, refs: &[ReferenceUse]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO "references"
            (reference_id, file_id, source_symbol, scope_id, kind, text, name,
            receiver, arity,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column,
            resolved_symbol_id, resolved_confidence, resolved_strategy, resolved_provenance,
            binding_id)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)"#,
    )?;
    for r in refs {
        stmt.execute(params![
            r.id, r.file_id, r.source_symbol, r.scope_id, r.kind.as_str(),
            r.text, r.name, r.receiver, r.arity,
            r.range.start_byte, r.range.end_byte, r.range.start_line,
            r.range.start_column, r.range.end_line, r.range.end_column,
            r.resolved.as_ref().map(|rt| &rt.symbol_id),
            r.resolved.as_ref().map(|rt| rt.confidence.as_f32()),
            r.resolved.as_ref().map(|rt| rt.strategy.as_str()),
            r.resolved.as_ref().map(|rt| rt.provenance.as_str()),
            r.binding_id,
        ])?;
    }
    Ok(())
}

fn write_imports(conn: &Connection, imports: &[ImportDef]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO imports
           (import_id, file_id, kind, module, imported_name, local_name, alias,
            is_wildcard, is_relative,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"#,
    )?;
    for imp in imports {
        stmt.execute(params![
            imp.id, imp.file_id, imp.kind.as_str(), imp.module, imp.imported_name,
            imp.local_name, imp.alias, imp.is_wildcard as i32, imp.is_relative as i32,
            imp.range.start_byte, imp.range.end_byte, imp.range.start_line,
            imp.range.start_column, imp.range.end_line, imp.range.end_column,
        ])?;
    }
    Ok(())
}

fn write_edges(conn: &Connection, edges: &[RawEdge]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO symbol_edges
           (edge_id, source, target, kind, confidence, provenance,
            ref_id, location_0, location_1, location_2, location_3, location_4, location_5,
            metadata, resolved_by)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"#,
    )?;
    for e in edges {
        let (loc_0, loc_1, loc_2, loc_3, loc_4, loc_5) = match &e.location {
            Some(loc) => (
                Some(loc.start_byte),
                Some(loc.end_byte),
                Some(loc.start_line),
                Some(loc.start_column),
                Some(loc.end_line),
                Some(loc.end_column),
            ),
            None => (None, None, None, None, None, None),
        };
        stmt.execute(params![
            e.id, e.source, e.target, e.kind.as_str(),
            e.confidence.as_f32(),
            e.provenance.as_str(),
            e.ref_id,
            loc_0, loc_1, loc_2, loc_3, loc_4, loc_5,
            e.metadata,
            e.resolved_by.as_ref().map(|s| s.as_str()),
        ])?;
    }
    Ok(())
}

fn write_callsites(conn: &Connection, callsites: &[Callsite]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO callsites
           (callsite_id, reference_id, caller, callee, receiver, args_json,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"#,
    )?;
    for cs in callsites {
        let args_json = serde_json::to_string(&cs.args)?;
        stmt.execute(params![
            cs.id, cs.reference_id, cs.caller, cs.callee, cs.receiver, args_json,
            cs.range.start_byte, cs.range.end_byte, cs.range.start_line,
            cs.range.start_column, cs.range.end_line, cs.range.end_column,
        ])?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// P3: Binding + Dataflow — row mappers
// ---------------------------------------------------------------------------

fn row_to_binding(row: &rusqlite::Row) -> rusqlite::Result<BindingDef> {
    Ok(BindingDef {
        id: row.get(0)?,
        file_id: row.get(1)?,
        function_id: row.get(2)?,
        scope_id: row.get(3)?,
        kind: BindingKind::from_str(row.get::<_, String>(4)?.as_str()).unwrap_or(BindingKind::Local),
        name: row.get(5)?,
        symbol_id: row.get(6)?,
        range: TextRange {
            start_byte: row.get(7)?,
            end_byte: row.get(8)?,
            start_line: row.get(9)?,
            start_column: row.get(10)?,
            end_line: row.get(11)?,
            end_column: row.get(12)?,
        },
    })
}

fn row_to_binding_use(row: &rusqlite::Row) -> rusqlite::Result<BindingUse> {
    Ok(BindingUse {
        id: row.get(0)?,
        file_id: row.get(1)?,
        scope_id: row.get(2)?,
        binding_id: row.get(3)?,
        reference_id: row.get(4)?,
        name: row.get(5)?,
        range: TextRange {
            start_byte: row.get(6)?,
            end_byte: row.get(7)?,
            start_line: row.get(8)?,
            start_column: row.get(9)?,
            end_line: row.get(10)?,
            end_column: row.get(11)?,
        },
    })
}

fn row_to_data_node(row: &rusqlite::Row) -> rusqlite::Result<DataNode> {
    Ok(DataNode {
        id: row.get(0)?,
        file_id: row.get(1)?,
        function_id: row.get(2)?,
        kind: DataNodeKind::from_str(row.get::<_, String>(3)?.as_str()).unwrap_or(DataNodeKind::Unknown),
        binding_id: row.get(4)?,
        callsite_id: row.get(5)?,
        name: row.get(6)?,
        access_path: row.get(7)?,
        range: TextRange {
            start_byte: row.get(8)?,
            end_byte: row.get(9)?,
            start_line: row.get(10)?,
            start_column: row.get(11)?,
            end_line: row.get(12)?,
            end_column: row.get(13)?,
        },
    })
}

fn row_to_dataflow_edge(row: &rusqlite::Row) -> rusqlite::Result<DataFlowEdge> {
    let location = TextRange {
        start_byte: row.get::<_, u32>(4).unwrap_or(0),
        end_byte: row.get::<_, u32>(5).unwrap_or(0),
        start_line: row.get::<_, u32>(6).unwrap_or(0),
        start_column: 0,
        end_line: 0,
        end_column: 0,
    };
    let conf: Option<f64> = row.get(7)?;
    Ok(DataFlowEdge {
        id: row.get(0)?,
        source: row.get(1)?,
        target: row.get(2)?,
        kind: DataFlowKind::from_str(row.get::<_, String>(3)?.as_str()).unwrap_or(DataFlowKind::Assign),
        location,
        confidence: conf.unwrap_or(0.8),
    })
}

// ---------------------------------------------------------------------------
// P3: Binding + Dataflow — write helpers
// ---------------------------------------------------------------------------

fn write_bindings(conn: &Connection, bindings: &[BindingDef]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO bindings
           (binding_id, file_id, function_id, scope_id, kind, name, symbol_id,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"#,
    )?;
    for b in bindings {
        stmt.execute(params![
            b.id, b.file_id, b.function_id, b.scope_id, b.kind.as_str(), b.name, b.symbol_id,
            b.range.start_byte, b.range.end_byte, b.range.start_line,
            b.range.start_column, b.range.end_line, b.range.end_column,
        ])?;
    }
    Ok(())
}

fn write_binding_uses(conn: &Connection, uses: &[BindingUse]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO binding_uses
           (binding_use_id, file_id, scope_id, binding_id, reference_id, name,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"#,
    )?;
    for u in uses {
        stmt.execute(params![
            u.id, u.file_id, u.scope_id, u.binding_id, u.reference_id, u.name,
            u.range.start_byte, u.range.end_byte, u.range.start_line,
            u.range.start_column, u.range.end_line, u.range.end_column,
        ])?;
    }
    Ok(())
}

fn write_data_nodes(conn: &Connection, nodes: &[DataNode]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO data_nodes
           (data_node_id, file_id, function_id, kind, binding_id, callsite_id,
            name, access_path,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"#,
    )?;
    for n in nodes {
        stmt.execute(params![
            n.id, n.file_id, n.function_id, n.kind.as_str(), n.binding_id, n.callsite_id,
            n.name, n.access_path,
            n.range.start_byte, n.range.end_byte, n.range.start_line,
            n.range.start_column, n.range.end_line, n.range.end_column,
        ])?;
    }
    Ok(())
}

fn write_dataflow_edges(conn: &Connection, edges: &[DataFlowEdge]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO dataflow_edges
           (dataflow_edge_id, source, target, kind, location_0, location_1, location_2, confidence)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"#,
    )?;
    for e in edges {
        stmt.execute(params![
            e.id, e.source, e.target, e.kind.as_str(),
            e.location.start_byte, e.location.end_byte, e.location.start_line,
            e.confidence,
        ])?;
    }
    Ok(())
}

fn write_callsite_args(conn: &Connection, args: &[CallsiteArg]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO callsite_args
           (callsite_id, index_, name, expr_text, data_node_id,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"#,
    )?;
    for a in args {
        stmt.execute(params![
            a.callsite_id, a.index, a.name, a.expr_text, a.data_node,
            a.range.start_byte, a.range.end_byte, a.range.start_line,
            a.range.start_column, a.range.end_line, a.range.end_column,
        ])?;
    }
    Ok(())
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
        store
            .update_reference_resolution(&ref_id, &target)
            .unwrap();

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
