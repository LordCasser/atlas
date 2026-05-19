//! Atlas-native SQLite schema DDL and migration system.
//!
//! Schema version: 1
//!
//! ## Tables
//! - `files`      — per-file metadata
//! - `symbols`    — all symbol definitions
//! - `scopes`     — containment regions
//! - `references` — all reference uses (preserved after resolution)
//! - `imports`    — import statements
//! - `edges`      — semantic edges with confidence/provenance
//! - `callsites`  — call expressions
//! - `project_metadata` — key-value project configuration
//! - `symbols_fts`— FTS5 index on symbol names
//! - `schema_versions` — migration tracking

/// Current schema version. Increment on every schema change.
pub const CURRENT_SCHEMA_VERSION: i64 = 3;

/// Minimum readable schema version (for backward compatibility).
pub const MIN_READABLE_VERSION: i64 = 1;
/// Complete DDL for a fresh database.
pub const SCHEMA_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS files (
    file_id       BLOB PRIMARY KEY NOT NULL,
    path          TEXT NOT NULL,
    language      TEXT NOT NULL,
    content_hash  TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'success',
    last_modified TEXT,
    index_time    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS symbols (
    symbol_id            BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL REFERENCES files(file_id) ON DELETE CASCADE,
    kind                 TEXT NOT NULL,
    name                 TEXT NOT NULL,
    qualified_name       TEXT NOT NULL,
    symbol_path_json     TEXT NOT NULL DEFAULT '[]',
    language             TEXT NOT NULL,
    -- code range (dual byte + line/column)
    range_start_byte     INTEGER NOT NULL,
    range_end_byte       INTEGER NOT NULL,
    range_start_line     INTEGER NOT NULL,
    range_start_column   INTEGER NOT NULL,
    range_end_line       INTEGER NOT NULL,
    range_end_column     INTEGER NOT NULL,
    -- name range
    name_start_byte      INTEGER NOT NULL,
    name_end_byte        INTEGER NOT NULL,
    name_start_line      INTEGER NOT NULL,
    name_start_column    INTEGER NOT NULL,
    name_end_line        INTEGER NOT NULL,
    name_end_column      INTEGER NOT NULL,
    signature            TEXT,
    visibility           TEXT,
    exported             INTEGER NOT NULL DEFAULT 0,
    static_              INTEGER NOT NULL DEFAULT 0,
    async_               INTEGER NOT NULL DEFAULT 0,
    container_id         BLOB REFERENCES symbols(symbol_id),
    scope_id             BLOB,
    package_name         TEXT,
    namespace_path_json  TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE IF NOT EXISTS scopes (
    scope_id             BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL REFERENCES files(file_id) ON DELETE CASCADE,
    kind                 TEXT NOT NULL,
    name                 TEXT NOT NULL,
    scope_path           TEXT NOT NULL,
    range_start_byte     INTEGER NOT NULL,
    range_end_byte       INTEGER NOT NULL,
    range_start_line     INTEGER NOT NULL,
    range_start_column   INTEGER NOT NULL,
    range_end_line       INTEGER NOT NULL,
    range_end_column     INTEGER NOT NULL,
    parent_id            BLOB REFERENCES scopes(scope_id)
);

CREATE TABLE IF NOT EXISTS references_v2 (
    reference_id         BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL REFERENCES files(file_id) ON DELETE CASCADE,
    source_symbol        BLOB,
    scope_id             BLOB,
    kind                 TEXT NOT NULL,
    text                 TEXT NOT NULL,
    name                 TEXT NOT NULL,
    receiver             TEXT,
    arity                INTEGER,
    -- reference range
    range_start_byte     INTEGER NOT NULL,
    range_end_byte       INTEGER NOT NULL,
    range_start_line     INTEGER NOT NULL,
    range_start_column   INTEGER NOT NULL,
    range_end_line       INTEGER NOT NULL,
    range_end_column     INTEGER NOT NULL,
    -- resolved target (NULL = unresolved, preserved)
    resolved_symbol_id   BLOB,
    resolved_confidence  REAL,
    resolved_strategy    TEXT,
    resolved_provenance  TEXT
);

CREATE TABLE IF NOT EXISTS imports (
    import_id            BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL REFERENCES files(file_id) ON DELETE CASCADE,
    kind                 TEXT NOT NULL,
    module               TEXT NOT NULL,
    imported_name        TEXT NOT NULL,
    local_name           TEXT,
    is_wildcard          INTEGER NOT NULL DEFAULT 0,
    is_relative          INTEGER NOT NULL DEFAULT 0,
    range_start_byte     INTEGER NOT NULL,
    range_end_byte       INTEGER NOT NULL,
    range_start_line     INTEGER NOT NULL,
    range_start_column   INTEGER NOT NULL,
    range_end_line       INTEGER NOT NULL,
    range_end_column     INTEGER NOT NULL,
    alias                TEXT
);

CREATE TABLE IF NOT EXISTS edges (
    edge_id      BLOB PRIMARY KEY NOT NULL,
    source       BLOB NOT NULL REFERENCES symbols(symbol_id) ON DELETE CASCADE,
    target       BLOB,
    kind         TEXT NOT NULL,
    confidence   REAL NOT NULL DEFAULT 0.5,
    provenance   TEXT NOT NULL DEFAULT 'tree_sitter',
    ref_id       BLOB,
    location_0   INTEGER,
    location_1   INTEGER,
    location_2   INTEGER,
    location_3   INTEGER,
    location_4   INTEGER,
    location_5   INTEGER,
    metadata     TEXT,
    resolved_by  TEXT
);

CREATE TABLE IF NOT EXISTS callsites (
    callsite_id          BLOB PRIMARY KEY NOT NULL,
    reference_id         BLOB,
    caller               BLOB NOT NULL REFERENCES symbols(symbol_id) ON DELETE CASCADE,
    callee               BLOB REFERENCES symbols(symbol_id) ON DELETE SET NULL,
    receiver             TEXT,
    args_json            TEXT NOT NULL DEFAULT '[]',
    range_start_byte     INTEGER NOT NULL,
    range_end_byte       INTEGER NOT NULL,
    range_start_line     INTEGER NOT NULL,
    range_start_column   INTEGER NOT NULL,
    range_end_line       INTEGER NOT NULL,
    range_end_column     INTEGER NOT NULL
);

-- FTS5 virtual table for symbol name search
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
    name,
    qualified_name,
    content='symbols',
    content_rowid='rowid'
);

-- Project-level metadata (key-value store for configuration, thresholds, timestamps)
CREATE TABLE IF NOT EXISTS project_metadata (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Schema version tracking
CREATE TABLE IF NOT EXISTS schema_versions (
    version     INTEGER PRIMARY KEY,
    applied_at  TEXT NOT NULL DEFAULT (datetime('now')),
    description TEXT
);

-- --- Indexes ---

CREATE INDEX IF NOT EXISTS idx_files_path
    ON files(path);
CREATE INDEX IF NOT EXISTS idx_files_language
    ON files(language);

CREATE INDEX IF NOT EXISTS idx_symbols_file
    ON symbols(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_qname
    ON symbols(qualified_name);
CREATE INDEX IF NOT EXISTS idx_symbols_kind
    ON symbols(kind);
CREATE INDEX IF NOT EXISTS idx_symbols_container
    ON symbols(container_id);

CREATE INDEX IF NOT EXISTS idx_scopes_file
    ON scopes(file_id);
CREATE INDEX IF NOT EXISTS idx_scopes_parent
    ON scopes(parent_id);

CREATE INDEX IF NOT EXISTS idx_references_file
    ON references_v2(file_id);
CREATE INDEX IF NOT EXISTS idx_references_source
    ON references_v2(source_symbol);
CREATE INDEX IF NOT EXISTS idx_references_resolved
    ON references_v2(resolved_symbol_id);
CREATE INDEX IF NOT EXISTS idx_references_unresolved
    ON references_v2(resolved_symbol_id) WHERE resolved_symbol_id IS NULL;

CREATE INDEX IF NOT EXISTS idx_imports_file
    ON imports(file_id);
CREATE INDEX IF NOT EXISTS idx_imports_module
    ON imports(module);

CREATE INDEX IF NOT EXISTS idx_edges_source
    ON edges(source);
CREATE INDEX IF NOT EXISTS idx_edges_target
    ON edges(target);
CREATE INDEX IF NOT EXISTS idx_edges_kind
    ON edges(kind);
CREATE INDEX IF NOT EXISTS idx_edges_source_kind
    ON edges(source, kind);

CREATE INDEX IF NOT EXISTS idx_callsites_caller
    ON callsites(caller);
CREATE INDEX IF NOT EXISTS idx_callsites_callee
    ON callsites(callee);

-- --- FTS Triggers ---

CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
    INSERT INTO symbols_fts(rowid, name, qualified_name)
    VALUES (new.rowid, new.name, new.qualified_name);
END;

CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, qualified_name)
    VALUES ('delete', old.rowid, old.name, old.qualified_name);
END;

CREATE TRIGGER IF NOT EXISTS symbols_au AFTER UPDATE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, qualified_name)
    VALUES ('delete', old.rowid, old.name, old.qualified_name);
    INSERT INTO symbols_fts(rowid, name, qualified_name)
    VALUES (new.rowid, new.name, new.qualified_name);
END;
"#;

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    #[test]
    fn test_schema_creation() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(super::SCHEMA_DDL).unwrap();

        // Verify all tables exist
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"files".to_string()));
        assert!(tables.contains(&"symbols".to_string()));
        assert!(tables.contains(&"scopes".to_string()));
        assert!(tables.contains(&"references_v2".to_string()));
        assert!(tables.contains(&"imports".to_string()));
        assert!(tables.contains(&"edges".to_string()));
        assert!(tables.contains(&"callsites".to_string()));
        assert!(tables.contains(&"symbols_fts".to_string()));
        assert!(tables.contains(&"project_metadata".to_string()));
        assert!(tables.contains(&"schema_versions".to_string()));
    }
}
