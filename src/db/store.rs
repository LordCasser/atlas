//! Atlas Store — the single SQLite persistence layer.
//!
//! `Store` wraps a `Mutex<Connection>` and provides all CRUD operations.
//! For MVP, a single writer/reader suffices. Future: split into `StoreWriter`
//! and `StoreReader` for concurrent read access.

use crate::db::schema::{CURRENT_SCHEMA_VERSION, SCHEMA_DDL};

use crate::types::*;
use rusqlite::{params, Connection, Transaction};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Thread-safe SQLite persistence layer.
pub struct Store {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl Store {
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
            conn: Mutex::new(conn),
            db_path,
        })
    }

    /// Open in-memory (for tests).
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: PathBuf::from(":memory:"),
        })
    }

    /// Initialize the schema (idempotent).
    pub fn init_schema(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(SCHEMA_DDL)?;

        // Record current version if not already present
        conn.execute(
            "INSERT OR IGNORE INTO schema_versions (version, description)
             VALUES (?1, ?2)",
            params![CURRENT_SCHEMA_VERSION, "Atlas-native schema v1"],
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
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // Files
    // -----------------------------------------------------------------------

    /// Insert or update a file record.
    pub fn upsert_file(&self, file: &FileInfo) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM files WHERE file_id = ?1", params![file_id])?;
        Ok(())
    }

    /// Get file info by ID.
    pub fn get_file(&self, file_id: &FileId) -> anyhow::Result<Option<FileInfo>> {
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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

    /// Find a symbol by ID.
    pub fn find_symbol_by_id(&self, id: &SymbolId) -> anyhow::Result<Option<SymbolDef>> {
        let conn = self.conn.lock().unwrap();
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

    /// Find all symbols in a file.
    pub fn find_symbols_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.conn.lock().unwrap();
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

    /// FTS5 search by name.
    pub fn search_symbols(&self, query: &str) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.conn.lock().unwrap();
        // Escape FTS5 special characters in query
        let safe_query = sanitize_fts5_query(query);
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
               JOIN symbols_fts fts ON s.rowid = fts.rowid
               WHERE symbols_fts MATCH ?1
               ORDER BY rank
               LIMIT 50"#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![safe_query], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(REFERENCE_SELECT_WHERE)?;
        let rows = stmt.query_map(params![file_id], row_to_reference)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find unresolved references (no resolved target).
    pub fn find_unresolved_references(&self) -> anyhow::Result<Vec<ReferenceUse>> {
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE references_v2 SET
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
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT edge_id, source, target, kind, confidence, provenance
             FROM edges WHERE source = ?1",
        )?;
        let rows = stmt.query_map(params![source], row_to_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find edges targeting a symbol.
    pub fn find_edges_by_target(&self, target: &SymbolId) -> anyhow::Result<Vec<RawEdge>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT edge_id, source, target, kind, confidence, provenance
             FROM edges WHERE target = ?1",
        )?;
        let rows = stmt.query_map(params![target], row_to_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find all imports for a file.
    pub fn find_imports_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<ImportDef>> {
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
    pub fn get_all_symbols(&self) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.conn.lock().unwrap();
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
    pub fn get_all_edges(&self) -> anyhow::Result<Vec<RawEdge>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT edge_id, source, target, kind, confidence, provenance FROM edges",
        )?;
        let rows = stmt.query_map([], row_to_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // FileFacts — convenience batch insert
    // -----------------------------------------------------------------------

    /// Insert all components of a `FileFacts` in a single transaction.
    /// This is the primary write path from extraction.
    pub fn insert_file_facts(&self, facts: &FileFacts) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
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
        if !facts.raw_edges.is_empty() {
            write_edges(&tx, &facts.raw_edges)?;
        }
        if !facts.callsites.is_empty() {
            write_callsites(&tx, &facts.callsites)?;
        }

        tx.commit()?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Stats
    // -----------------------------------------------------------------------

    /// Collection metrics about the indexed codebase.
    pub fn get_stats(&self) -> anyhow::Result<StoreStats> {
        let conn = self.conn.lock().unwrap();
        let total_files: i64 =
            conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let total_symbols: i64 =
            conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        let total_edges: i64 =
            conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
        let total_references: i64 =
            conn.query_row("SELECT COUNT(*) FROM references_v2", [], |r| r.get(0))?;
        let unresolved: i64 = conn.query_row(
            "SELECT COUNT(*) FROM references_v2 WHERE resolved_symbol_id IS NULL",
            [],
            |r| r.get(0),
        )?;
        let sqlite_version: String =
            conn.query_row("SELECT sqlite_version()", [], |r| r.get(0))?;

        Ok(StoreStats {
            total_files,
            total_symbols,
            total_edges,
            total_references,
            unresolved_references: unresolved,
            sqlite_version,
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
           resolved_provenance
    FROM references_v2"#;

const REFERENCE_SELECT_WHERE: &str = r#"
    SELECT reference_id, file_id, source_symbol, scope_id, kind,
           text, name, receiver, arity,
           range_start_byte, range_end_byte, range_start_line,
           range_start_column, range_end_line, range_end_column,
           resolved_symbol_id, resolved_confidence, resolved_strategy,
           resolved_provenance
    FROM references_v2 WHERE file_id = ?1"#;

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
    })
}

fn row_to_edge(row: &rusqlite::Row) -> rusqlite::Result<RawEdge> {
    Ok(RawEdge {
        id: row.get(0)?,
        source: row.get(1)?,
        target: row.get(2)?,
        kind: EdgeKind::from_str(row.get::<_, String>(3)?.as_str()).unwrap_or(EdgeKind::References),
        confidence: Confidence::new(row.get(4)?),
        provenance: Provenance::from_str(row.get::<_, String>(5)?.as_str())
            .unwrap_or_default(),
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
        let path_json = serde_json::to_string(&sc.scope_path)?;
        stmt.execute(params![
            sc.id, sc.file_id, sc.kind.as_str(), sc.name, path_json, sc.parent_id,
            sc.range.start_byte, sc.range.end_byte, sc.range.start_line,
            sc.range.start_column, sc.range.end_line, sc.range.end_column,
        ])?;
    }
    Ok(())
}

fn write_references(conn: &Connection, refs: &[ReferenceUse]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO references_v2
            (reference_id, file_id, source_symbol, scope_id, kind, text, name,
            receiver, arity,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column,
            resolved_symbol_id, resolved_confidence, resolved_strategy, resolved_provenance)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)"#,
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
        r#"INSERT OR REPLACE INTO edges
           (edge_id, source, target, kind, confidence, provenance)
        VALUES (?1,?2,?3,?4,?5,?6)"#,
    )?;
    for e in edges {
        stmt.execute(params![
            e.id, e.source, e.target, e.kind.as_str(),
            e.confidence.as_f32(),
            e.provenance.as_str(),
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
