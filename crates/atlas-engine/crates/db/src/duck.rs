//! DuckDB backend — high-throughput bulk-write replacement for SQLite.
//!
//! DuckDB's columnar storage and Appender API (bypasses SQL parsing) make
//! bulk inserts 10-100× faster than SQLite for the dense row-per-file
//! patterns of full-analysis extraction.
//!
//! ## Usage (exploratory)
//!
//! ```ignore
//! let duck = DuckStore::open_in_memory()?;
//! duck.init_schema()?;
//! duck.insert_symbols(&symbols)?;  // Appender → 5M rows/s
//! ```

use std::path::Path;
use types::*;

use duckdb::{params, Connection};

/// Minimal DuckDB-backed store for bulk writes.
pub struct DuckStore {
    conn: Connection,
}

impl DuckStore {
    /// Open an in-memory DuckDB database.
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        // Performance pragmas
        conn.execute_batch(
            "SET threads = 4;
             SET memory_limit = '4GB';
             SET preserve_insertion_order = false;",
        )?;
        Ok(Self { conn })
    }

    /// Open a file-backed DuckDB database.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "SET threads = 4;
             SET memory_limit = '4GB';
             SET preserve_insertion_order = false;",
        )?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    // ── Schema ──────────────────────────────────────────────────────────

    /// Create the atlas schema in DuckDB (PostgreSQL-compatible syntax).
    pub fn init_schema(&self) -> anyhow::Result<()> {
        self.conn.execute_batch(
            r#"
            -- Core tables (mirror SQLite schema with DuckDB types)
            CREATE TABLE IF NOT EXISTS files (
                file_id     BLOB PRIMARY KEY,
                path        TEXT NOT NULL,
                language    TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                status      TEXT NOT NULL DEFAULT 'active',
                index_time  TIMESTAMP DEFAULT now()
            );

            CREATE TABLE IF NOT EXISTS symbols (
                symbol_id           BLOB PRIMARY KEY,
                file_id             BLOB NOT NULL,
                kind                TEXT NOT NULL,
                name                TEXT NOT NULL,
                qualified_name      TEXT NOT NULL DEFAULT '',
                symbol_path_json    TEXT NOT NULL DEFAULT '[]',
                language            TEXT NOT NULL,
                range_start_byte    BIGINT NOT NULL DEFAULT 0,
                range_end_byte      BIGINT NOT NULL DEFAULT 0,
                range_start_line    BIGINT NOT NULL DEFAULT 0,
                range_start_column  BIGINT NOT NULL DEFAULT 0,
                range_end_line      BIGINT NOT NULL DEFAULT 0,
                range_end_column    BIGINT NOT NULL DEFAULT 0,
                name_start_byte     BIGINT NOT NULL DEFAULT 0,
                name_end_byte       BIGINT NOT NULL DEFAULT 0,
                name_start_line     BIGINT NOT NULL DEFAULT 0,
                name_start_column   BIGINT NOT NULL DEFAULT 0,
                name_end_line       BIGINT NOT NULL DEFAULT 0,
                name_end_column     BIGINT NOT NULL DEFAULT 0,
                signature           TEXT,
                visibility          TEXT,
                exported            BOOLEAN NOT NULL DEFAULT false,
                static_             BOOLEAN NOT NULL DEFAULT false,
                async_              BOOLEAN NOT NULL DEFAULT false,
                container_id        BLOB,
                scope_id            BLOB,
                package_name        TEXT,
                namespace_path_json TEXT NOT NULL DEFAULT '[]',
                layer               TEXT NOT NULL DEFAULT 'manifest'
            );

            CREATE TABLE IF NOT EXISTS scopes (
                scope_id    BLOB PRIMARY KEY,
                file_id     BLOB NOT NULL,
                kind        TEXT NOT NULL,
                name        TEXT,
                parent_id   BLOB,
                range_start_byte   BIGINT NOT NULL DEFAULT 0,
                range_end_byte     BIGINT NOT NULL DEFAULT 0,
                range_start_line   BIGINT NOT NULL DEFAULT 0,
                range_start_column BIGINT NOT NULL DEFAULT 0,
                range_end_line     BIGINT NOT NULL DEFAULT 0,
                range_end_column   BIGINT NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS "references" (
                reference_id        BLOB PRIMARY KEY,
                file_id             BLOB NOT NULL,
                source_symbol_id    BLOB,
                name                TEXT NOT NULL,
                kind                TEXT NOT NULL,
                scope_id            BLOB,
                range_start_byte    BIGINT NOT NULL DEFAULT 0,
                range_end_byte      BIGINT NOT NULL DEFAULT 0,
                range_start_line    BIGINT NOT NULL DEFAULT 0,
                range_start_column  BIGINT NOT NULL DEFAULT 0,
                range_end_line      BIGINT NOT NULL DEFAULT 0,
                range_end_column    BIGINT NOT NULL DEFAULT 0,
                resolved_symbol_id  BLOB,
                resolved_confidence REAL,
                resolved_strategy   TEXT,
                resolved_provenance TEXT
            );

            CREATE TABLE IF NOT EXISTS imports (
                import_id       BLOB PRIMARY KEY,
                file_id         BLOB NOT NULL,
                module_path     TEXT NOT NULL,
                imported_name   TEXT NOT NULL,
                kind            TEXT NOT NULL,
                range_start_byte    BIGINT NOT NULL DEFAULT 0,
                range_end_byte      BIGINT NOT NULL DEFAULT 0,
                range_start_line    BIGINT NOT NULL DEFAULT 0,
                range_start_column  BIGINT NOT NULL DEFAULT 0,
                range_end_line      BIGINT NOT NULL DEFAULT 0,
                range_end_column    BIGINT NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS symbol_edges (
                edge_id     BLOB PRIMARY KEY,
                source      BLOB NOT NULL,
                target      BLOB NOT NULL,
                kind        TEXT NOT NULL,
                confidence  REAL,
                provenance  TEXT NOT NULL DEFAULT 'tree-sitter'
            );
            "#,
        )?;
        Ok(())
    }

    // ── Bulk write helpers ─────────────────────────────────────────────

    /// Bulk-insert symbols using DuckDB's high-throughput Appender.
    pub fn insert_symbols(&self, symbols: &[SymbolDef], layer: &str) -> anyhow::Result<()> {
        if symbols.is_empty() {
            return Ok(());
        }
        let mut app = self.conn.appender("symbols")?;
        for sym in symbols {
            let scope_id: Option<&[u8]> = sym.scope_id.as_ref().map(|s| s.as_bytes().as_slice());
            let container_id: Option<&[u8]> = sym.container.as_ref().map(|c| c.as_bytes().as_slice());
            let visibility = sym.visibility.as_ref().map(|v| v.as_str()).unwrap_or("");
            let signature = sym.signature.as_deref().unwrap_or("");
            let pkg = sym.package_name.as_deref().unwrap_or("");
            app.append_row(params![
                sym.id.as_bytes().as_slice(),
                sym.file_id.as_bytes().as_slice(),
                sym.kind.as_str(),
                sym.name.as_str(),
                sym.qualified_name.as_str(),
                "[]",
                sym.language.as_str(),
                sym.range.start_byte as i64,
                sym.range.end_byte as i64,
                sym.range.start_line as i64,
                sym.range.start_column as i64,
                sym.range.end_line as i64,
                sym.range.end_column as i64,
                sym.name_range.start_byte as i64,
                sym.name_range.end_byte as i64,
                sym.name_range.start_line as i64,
                sym.name_range.start_column as i64,
                sym.name_range.end_line as i64,
                sym.name_range.end_column as i64,
                signature,
                visibility,
                sym.exported,
                sym.static_,
                sym.async_,
                container_id,
                scope_id,
                pkg,
                "[]",
                layer,
            ])?;
        }
        Ok(())
    }

    /// Bulk-insert scopes.
    pub fn insert_scopes(&self, scopes: &[ScopeDef]) -> anyhow::Result<()> {
        if scopes.is_empty() {
            return Ok(());
        }
        let mut app = self.conn.appender("scopes")?;
        for scope in scopes {
            let parent_id: Option<&[u8]> = scope.parent_id.as_ref().map(|p| p.as_bytes().as_slice());
            let name = scope.name.as_str();
            app.append_row(params![
                scope.id.as_bytes().as_slice(),
                scope.file_id.as_bytes().as_slice(),
                scope.kind.as_str(),
                name,
                parent_id,
                scope.range.start_byte as i64,
                scope.range.end_byte as i64,
                scope.range.start_line as i64,
                scope.range.start_column as i64,
                scope.range.end_line as i64,
                scope.range.end_column as i64,
            ])?;
        }
        Ok(())
    }

    /// Bulk-insert references.
    pub fn insert_references(&self, refs: &[ReferenceUse]) -> anyhow::Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        let mut app = self.conn.appender("\"references\"")?;
        for r in refs {
            let source_id: Option<&[u8]> = r.source_symbol.as_ref().map(|s| s.as_bytes().as_slice());
            let scope_id: Option<&[u8]> = r.scope_id.as_ref().map(|s| s.as_bytes().as_slice());
            app.append_row(params![
                r.id.as_bytes().as_slice(),
                r.file_id.as_bytes().as_slice(),
                source_id,
                r.name.as_str(),
                r.kind.as_str(),
                scope_id,
                r.range.start_byte as i64,
                r.range.end_byte as i64,
                r.range.start_line as i64,
                r.range.start_column as i64,
                r.range.end_line as i64,
                r.range.end_column as i64,
                Option::<&[u8]>::None,
                Option::<f64>::None,
                Option::<&str>::None,
                Option::<&str>::None,
            ])?;
        }
        Ok(())
    }

    /// Bulk-insert imports.
    pub fn insert_imports(&self, imports: &[ImportDef]) -> anyhow::Result<()> {
        if imports.is_empty() {
            return Ok(());
        }
        let mut app = self.conn.appender("imports")?;
        for imp in imports {
            app.append_row(params![
                imp.id.as_bytes().as_slice(),
                imp.file_id.as_bytes().as_slice(),
                imp.module.as_str(),
                imp.imported_name.as_str(),
                imp.kind.as_str(),
                imp.range.start_byte as i64,
                imp.range.end_byte as i64,
                imp.range.start_line as i64,
                imp.range.start_column as i64,
                imp.range.end_line as i64,
                imp.range.end_column as i64,
            ])?;
        }
        Ok(())
    }
}
