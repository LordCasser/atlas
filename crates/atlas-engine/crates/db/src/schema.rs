//! Atlas-native SQLite schema DDL — with migration infrastructure.
//!
//! The schema is currently at **V1**.  Schema changes during development
//! are made in-place.  Migration from V1 to future versions is supported
//! via the ordered [`MIGRATIONS`] chain.  When no migration path exists
//! (future→past downgrade or very old DB), the user is directed to
//! `atlas init` for a fresh rebuild.
//!
//! Schema version: 1
//!
//! ## Tables
//! - `files`          — per-file metadata
//! - `symbols`        — all symbol definitions
//! - `scopes`         — containment regions
//! - `references`     — all reference uses (preserved after resolution)
//! - `imports`        — import statements
//! - `symbol_edges`   — semantic edges between symbols (renamed from edges)
//! - `callsites`      — call expressions
//! - `bindings`       — lexical binding definitions
//! - `binding_uses`   — references to bindings
//! - `data_nodes`     — dataflow nodes
//! - `dataflow_edges` — dataflow edges between DataNodes
//! - `cfg_nodes`      — control-flow graph nodes per function
//! - `cfg_edges`      — control-flow graph edges
//! - `project_metadata` — key-value project configuration
//! - `symbols_fts`    — FTS5 index on symbol names
//! - `schema_versions` — migration tracking

/// Current schema version.
///
/// When this value is raised, add matching entries to [`MIGRATIONS`] so
/// existing databases can be upgraded or explicitly reported as needing a
/// rebuild.
pub const CURRENT_SCHEMA_VERSION: i64 = 1;
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

CREATE TABLE IF NOT EXISTS "references" (
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
    resolved_provenance  TEXT,
    -- lexical binding link (filled by SemanticBinder after extraction)
    binding_id           BLOB
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

CREATE TABLE IF NOT EXISTS symbol_edges (
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
    range_end_column     INTEGER NOT NULL,
    callee_start_line    INTEGER,
    callee_start_column  INTEGER,
    callee_end_line      INTEGER,
    callee_end_column    INTEGER,
    callee_start_byte    INTEGER,
    callee_end_byte      INTEGER
);

-- ===== Binding + Dataflow tables =====

CREATE TABLE IF NOT EXISTS bindings (
    binding_id           BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL REFERENCES files(file_id) ON DELETE CASCADE,
    function_id          BLOB REFERENCES symbols(symbol_id) ON DELETE CASCADE,
    scope_id             BLOB NOT NULL REFERENCES scopes(scope_id) ON DELETE CASCADE,
    kind                 TEXT NOT NULL,
    name                 TEXT NOT NULL,
    symbol_id            BLOB REFERENCES symbols(symbol_id) ON DELETE SET NULL,
    range_start_byte     INTEGER NOT NULL,
    range_end_byte       INTEGER NOT NULL,
    range_start_line     INTEGER NOT NULL,
    range_start_column   INTEGER NOT NULL,
    range_end_line       INTEGER NOT NULL,
    range_end_column     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS binding_uses (
    binding_use_id       BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL REFERENCES files(file_id) ON DELETE CASCADE,
    scope_id             BLOB REFERENCES scopes(scope_id),
    binding_id           BLOB REFERENCES bindings(binding_id) ON DELETE SET NULL,
    reference_id         BLOB REFERENCES "references"(reference_id) ON DELETE SET NULL,
    name                 TEXT NOT NULL,
    range_start_byte     INTEGER NOT NULL,
    range_end_byte       INTEGER NOT NULL,
    range_start_line     INTEGER NOT NULL,
    range_start_column   INTEGER NOT NULL,
    range_end_line       INTEGER NOT NULL,
    range_end_column     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS data_nodes (
    data_node_id         BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL REFERENCES files(file_id) ON DELETE CASCADE,
    function_id          BLOB REFERENCES symbols(symbol_id) ON DELETE CASCADE,
    kind                 TEXT NOT NULL,
    binding_id           BLOB REFERENCES bindings(binding_id) ON DELETE SET NULL,
    callsite_id          BLOB,
    name                 TEXT,
    access_path          TEXT,
    arg_index            INTEGER,
    range_start_byte     INTEGER NOT NULL,
    range_end_byte       INTEGER NOT NULL,
    range_start_line     INTEGER NOT NULL,
    range_start_column   INTEGER NOT NULL,
    range_end_line       INTEGER NOT NULL,
    range_end_column     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS dataflow_edges (
    dataflow_edge_id     BLOB PRIMARY KEY NOT NULL,
    source               BLOB NOT NULL REFERENCES data_nodes(data_node_id) ON DELETE CASCADE,
    target               BLOB NOT NULL REFERENCES data_nodes(data_node_id) ON DELETE CASCADE,
    kind                 TEXT NOT NULL,
    location_0           INTEGER,
    location_1           INTEGER,
    location_2           INTEGER,
    location_3           INTEGER,
    location_4           INTEGER,
    location_5           INTEGER,
    confidence           REAL NOT NULL DEFAULT 0.8
);

-- cfg_nodes: per-function control-flow graph nodes
CREATE TABLE IF NOT EXISTS cfg_nodes (
    cfg_node_id          BLOB PRIMARY KEY NOT NULL,
    function_id          BLOB NOT NULL REFERENCES symbols(symbol_id) ON DELETE CASCADE,
    kind                 TEXT NOT NULL,            -- entry/exit/statement/branch/loop/return/throw/join
    range_start_byte     INTEGER NOT NULL,
    range_end_byte       INTEGER NOT NULL,
    range_start_line     INTEGER NOT NULL,
    range_start_column   INTEGER NOT NULL,
    range_end_line       INTEGER NOT NULL,
    range_end_column     INTEGER NOT NULL
);

-- cfg_edges: control-flow edges between CFG nodes
CREATE TABLE IF NOT EXISTS cfg_edges (
    cfg_edge_id          BLOB PRIMARY KEY NOT NULL,
    source_node          BLOB NOT NULL REFERENCES cfg_nodes(cfg_node_id) ON DELETE CASCADE,
    target_node          BLOB NOT NULL REFERENCES cfg_nodes(cfg_node_id) ON DELETE CASCADE,
    kind                 TEXT NOT NULL
);

-- Lazy dataflow artifact tracking: records which AnalysisUnits
-- have had their dataflow/CFG built and persisted.
CREATE TABLE IF NOT EXISTS analysis_artifacts (
    file_id         BLOB NOT NULL,
    unit_id         BLOB NOT NULL,
    layer           TEXT NOT NULL,      -- 'dataflow' | 'cfg'
    content_hash    TEXT NOT NULL,      -- file content_hash at build time
    status          TEXT NOT NULL DEFAULT 'complete',
    node_count      INTEGER,
    edge_count      INTEGER,
    budget_exceeded INTEGER NOT NULL DEFAULT 0,
    built_at        TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (file_id, unit_id, layer),
    FOREIGN KEY (file_id) REFERENCES files(file_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_artifacts_file
    ON analysis_artifacts(file_id);

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

-- Schema single-version marker (no migration — always V1)
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
    ON "references"(file_id);
CREATE INDEX IF NOT EXISTS idx_references_source
    ON "references"(source_symbol);
CREATE INDEX IF NOT EXISTS idx_references_resolved
    ON "references"(resolved_symbol_id);
CREATE INDEX IF NOT EXISTS idx_references_unresolved
    ON "references"(resolved_symbol_id) WHERE resolved_symbol_id IS NULL;

CREATE INDEX IF NOT EXISTS idx_imports_file
    ON imports(file_id);
CREATE INDEX IF NOT EXISTS idx_imports_module
    ON imports(module);

CREATE INDEX IF NOT EXISTS idx_symbol_edges_source
    ON symbol_edges(source);
CREATE INDEX IF NOT EXISTS idx_symbol_edges_target
    ON symbol_edges(target);
CREATE INDEX IF NOT EXISTS idx_symbol_edges_kind
    ON symbol_edges(kind);
CREATE INDEX IF NOT EXISTS idx_symbol_edges_source_kind
    ON symbol_edges(source, kind);

CREATE INDEX IF NOT EXISTS idx_callsites_caller
    ON callsites(caller);
CREATE INDEX IF NOT EXISTS idx_callsites_callee
    ON callsites(callee);

-- Binding + Dataflow indexes
CREATE INDEX IF NOT EXISTS idx_bindings_file
    ON bindings(file_id);
CREATE INDEX IF NOT EXISTS idx_bindings_function
    ON bindings(function_id);
CREATE INDEX IF NOT EXISTS idx_bindings_symbol
    ON bindings(symbol_id);

CREATE INDEX IF NOT EXISTS idx_binding_uses_file
    ON binding_uses(file_id);
CREATE INDEX IF NOT EXISTS idx_binding_uses_binding
    ON binding_uses(binding_id);
CREATE INDEX IF NOT EXISTS idx_binding_uses_reference
    ON binding_uses(reference_id);

CREATE INDEX IF NOT EXISTS idx_data_nodes_file
    ON data_nodes(file_id);
CREATE INDEX IF NOT EXISTS idx_data_nodes_function
    ON data_nodes(function_id);
CREATE INDEX IF NOT EXISTS idx_data_nodes_binding
    ON data_nodes(binding_id);

CREATE INDEX IF NOT EXISTS idx_dataflow_edges_source
    ON dataflow_edges(source);
CREATE INDEX IF NOT EXISTS idx_dataflow_edges_target
    ON dataflow_edges(target);
CREATE INDEX IF NOT EXISTS idx_dataflow_edges_kind
    ON dataflow_edges(kind);

CREATE INDEX IF NOT EXISTS idx_cfg_nodes_function
    ON cfg_nodes(function_id);
CREATE INDEX IF NOT EXISTS idx_cfg_nodes_kind
    ON cfg_nodes(kind);

CREATE INDEX IF NOT EXISTS idx_cfg_edges_source
    ON cfg_edges(source_node);
CREATE INDEX IF NOT EXISTS idx_cfg_edges_target
    ON cfg_edges(target_node);
CREATE INDEX IF NOT EXISTS idx_cfg_edges_kind
    ON cfg_edges(kind);

-- Optimized lookups for reader traits (trace/analysis hot paths).
CREATE INDEX IF NOT EXISTS idx_symbols_name
    ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_files_path
    ON files(path);

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

// ---------------------------------------------------------------------------
// Migration infrastructure
// ---------------------------------------------------------------------------

/// A single schema migration step.
///
/// Each entry in [`MIGRATIONS`] upgrades the database from `from_version`
/// to `from_version + 1`.  Migrations are applied in order and must be
/// idempotent (using `IF NOT EXISTS` / `IF EXISTS` where possible).
pub struct Migration {
    /// Source version this migration upgrades FROM.
    pub from_version: i64,
    /// SQL DDL to execute in a single transaction.
    pub sql: &'static str,
    /// Human-readable description (recorded in `schema_versions`).
    pub description: &'static str,
}

/// Ordered migration chain.
///
/// When the database is at version N and [`CURRENT_SCHEMA_VERSION`] is M > N,
/// migrations from index N-1 through M-1 are applied sequentially.  When no
/// migration covers the gap, the user is directed to `atlas init`.
///
/// Add entries here when the schema changes:
/// ```ignore
/// pub const MIGRATIONS: &[Migration] = &[
///     Migration { from_version: 1, sql: "ALTER TABLE ...", description: "v2: ..." },
///     Migration { from_version: 2, sql: "CREATE INDEX ...", description: "v3: ..." },
/// ];
/// ```
pub const MIGRATIONS: &[Migration] = &[
    // V1→V2 example (uncomment and fill when needed):
    // Migration {
    //     from_version: 1,
    //     sql: "ALTER TABLE symbols ADD COLUMN new_field TEXT;",
    //     description: "v2: add new_field to symbols",
    // },
];

/// Run pending migrations on a database connection.
///
/// Reads the current version from `schema_versions`, applies all migrations
/// whose `from_version >= current_version` and < [`CURRENT_SCHEMA_VERSION`],
/// and records each applied migration.
///
/// Returns the number of migrations applied.
pub fn run_migrations(conn: &rusqlite::Connection) -> anyhow::Result<usize> {
    let current = current_schema_version(conn)?;
    if current >= CURRENT_SCHEMA_VERSION {
        return Ok(0); // up-to-date or newer (handled by check_schema_compat)
    }

    let mut applied = 0usize;
    for mig in MIGRATIONS {
        if mig.from_version < current {
            continue; // already applied
        }
        if mig.from_version >= CURRENT_SCHEMA_VERSION {
            break; // past target
        }
        // Only apply if this migration bridges from exactly where we are
        if mig.from_version != current + applied as i64 {
            anyhow::bail!(
                "No migration from v{} to v{}; run `atlas init` to rebuild",
                current + applied as i64,
                CURRENT_SCHEMA_VERSION,
            );
        }
        conn.execute_batch(mig.sql)?;
        conn.execute(
            "INSERT INTO schema_versions (version, description) VALUES (?1, ?2)",
            rusqlite::params![mig.from_version + 1, mig.description],
        )?;
        applied += 1;
    }
    Ok(applied)
}

/// Read the current schema version from the database.
///
/// Returns 0 if the `schema_versions` table does not exist or is empty.
pub fn current_schema_version(conn: &rusqlite::Connection) -> anyhow::Result<i64> {
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_versions'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false);

    if !table_exists {
        return Ok(0);
    }

    let ver: Option<i64> = conn
        .query_row(
            "SELECT version FROM schema_versions ORDER BY version DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    Ok(ver.unwrap_or(0))
}

/// Result of checking schema compatibility on DB open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaStatus {
    /// Schema is current — no action needed.
    Current,
    /// Migrations were applied successfully.
    Migrated { from: i64, to: i64, steps: usize },
    /// DB is from a newer version — cannot proceed.
    TooNew { db_version: i64, app_version: i64 },
    /// DB needs migration but no path exists.
    NeedsRebuild { db_version: i64, app_version: i64 },
}

/// Check schema compatibility and run pending migrations if possible.
///
/// Called after opening a database.  Returns the current status and any
/// migration result.
pub fn check_and_migrate(conn: &rusqlite::Connection) -> anyhow::Result<SchemaStatus> {
    let db_ver = current_schema_version(conn)?;

    if db_ver == 0 {
        // Fresh DB or pre-versioning DB — will be initialized by init_schema
        return Ok(SchemaStatus::Current);
    }

    if db_ver > CURRENT_SCHEMA_VERSION {
        return Ok(SchemaStatus::TooNew {
            db_version: db_ver,
            app_version: CURRENT_SCHEMA_VERSION,
        });
    }

    if db_ver < CURRENT_SCHEMA_VERSION {
        let steps = run_migrations(conn)?;
        if steps > 0 {
            return Ok(SchemaStatus::Migrated {
                from: db_ver,
                to: CURRENT_SCHEMA_VERSION,
                steps,
            });
        }
        // No migration path available
        return Ok(SchemaStatus::NeedsRebuild {
            db_version: db_ver,
            app_version: CURRENT_SCHEMA_VERSION,
        });
    }

    Ok(SchemaStatus::Current)
}

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
        assert!(tables.contains(&"references".to_string()));
        assert!(tables.contains(&"imports".to_string()));
        assert!(tables.contains(&"symbol_edges".to_string()));
        assert!(tables.contains(&"callsites".to_string()));
        assert!(tables.contains(&"bindings".to_string()));
        assert!(tables.contains(&"binding_uses".to_string()));
        assert!(tables.contains(&"data_nodes".to_string()));
        assert!(tables.contains(&"dataflow_edges".to_string()));
        assert!(tables.contains(&"cfg_nodes".to_string()));
        assert!(tables.contains(&"cfg_edges".to_string()));
        assert!(tables.contains(&"symbols_fts".to_string()));
        assert!(tables.contains(&"project_metadata".to_string()));
        assert!(tables.contains(&"schema_versions".to_string()));
    }

    #[test]
    fn test_current_schema_version_on_fresh_db_is_zero() {
        let conn = Connection::open_in_memory().unwrap();
        // No schema_versions table → version 0
        let ver = super::current_schema_version(&conn).unwrap();
        assert_eq!(ver, 0);
    }

    #[test]
    fn test_check_and_migrate_current_is_noop() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(super::SCHEMA_DDL).unwrap();
        // Record current version (init_schema would do this)
        conn.execute(
            "INSERT INTO schema_versions (version, description) VALUES (?1, ?2)",
            rusqlite::params![super::CURRENT_SCHEMA_VERSION, "v1: test"],
        )
        .unwrap();

        let status = super::check_and_migrate(&conn).unwrap();
        assert_eq!(status, super::SchemaStatus::Current);
    }

    #[test]
    fn test_check_and_migrate_too_new_is_detected() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(super::SCHEMA_DDL).unwrap();
        conn.execute(
            "INSERT INTO schema_versions (version, description) VALUES (?1, ?2)",
            rusqlite::params![999, "future version"],
        )
        .unwrap();

        let status = super::check_and_migrate(&conn).unwrap();
        assert!(matches!(status, super::SchemaStatus::TooNew { .. }));
    }

    #[test]
    fn test_migrations_array_is_empty_at_v1() {
        // V1 is the current version — no migrations needed yet
        assert!(super::MIGRATIONS.is_empty());
    }
}
