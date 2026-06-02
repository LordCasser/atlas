//! Atlas-native SQLite schema DDL.
//!
//! Schema version: 1
//!
//! ## Tables
//! - `files`          — per-file metadata
//! - `symbols`        — all symbol definitions
//! - `scopes`         — containment regions
//! - `references`     — all reference uses (preserved after resolution)
//! - `imports`        — import statements
//! - `symbol_edges`   — semantic edges between symbols
//! - `callsites`      — call expressions
//! - `bindings`       — lexical binding definitions
//! - `binding_uses`   — references to bindings
//! - `data_nodes`     — dataflow nodes
//! - `dataflow_edges` — dataflow edges between DataNodes
//! - `cfg_nodes`      — control-flow graph nodes per function
//! - `cfg_edges`      — control-flow graph edges
//! - `function_summaries`        — per-function summary metadata
//! - `summary_param_reaches`     — parameter → downstream target reachability
//! - `summary_return_sources`    — return → upstream source mapping
//! - `summary_call_arg_sources`  — call argument → upstream source mapping
//! - `extraction_state` — unified file/unit extraction completion state
//! - `extraction_jobs`  — unified extraction job tracking (queued/building/complete/failed)
//! - `project_metadata` — key-value project configuration
//! - `function_pointer_annotations` — user-declared function-pointer dispatch annotations
//! - `domain_rules`   — user-defined and learned ownership rules for lifecycle analysis
//! - `symbols_fts`    — FTS5 index on symbol names

/// Current schema version.
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
    namespace_path_json  TEXT NOT NULL DEFAULT '[]',
    layer                TEXT NOT NULL DEFAULT 'structural'     -- manifest | resolution_symbols | structural
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
    -- no FK on source_symbol: source may be a file/module-scope symbol or external
    source_symbol        BLOB,
    -- no FK on scope_id: scope may be implicit or not yet resolved
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
    -- no FK on target: target may reference an external symbol, a deleted symbol,
    -- or a symbol that has not yet been indexed
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
    -- no FK on reference_id: callsite may be provisional (before reference is finalized)
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
    range_end_column     INTEGER NOT NULL,
    effect_kind          TEXT,                     -- read/write/allocate/free/call/condition/return/goto/assign
    target_field         TEXT,                     -- e.g. "data->state.aptr" (normalized struct field path)
    semantic_effects_json TEXT,                     -- serialized Vec<SemanticEffect> as JSON
    callee_name          TEXT                      -- callee function name for Call-effect nodes (nullable, for domain rule matching)
);

-- cfg_edges: control-flow edges between CFG nodes
CREATE TABLE IF NOT EXISTS cfg_edges (
    cfg_edge_id          BLOB PRIMARY KEY NOT NULL,
    source_node          BLOB NOT NULL REFERENCES cfg_nodes(cfg_node_id) ON DELETE CASCADE,
    target_node          BLOB NOT NULL REFERENCES cfg_nodes(cfg_node_id) ON DELETE CASCADE,
    kind                 TEXT NOT NULL
);

-- Unified extraction completion state.
--
-- unit_id IS NULL records file-level layers:
--   manifest | resolution_symbols | structural | dataflow
-- unit_id IS NOT NULL records AnalysisUnit-level layers:
--   dataflow | cfg
CREATE TABLE IF NOT EXISTS extraction_state (
    file_id         BLOB NOT NULL,
    unit_id         BLOB,
    layer           TEXT NOT NULL,
    content_hash    TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'complete',  -- complete | partial | failed
    node_count      INTEGER,
    edge_count      INTEGER,
    budget_exceeded INTEGER NOT NULL DEFAULT 0,
    capability_mask INTEGER NOT NULL DEFAULT 0,
    updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (file_id) REFERENCES files(file_id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_extraction_state_file_layer
    ON extraction_state(file_id, layer)
    WHERE unit_id IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_extraction_state_unit_layer
    ON extraction_state(file_id, unit_id, layer)
    WHERE unit_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_extraction_state_file
    ON extraction_state(file_id);

CREATE INDEX IF NOT EXISTS idx_extraction_state_layer_status
    ON extraction_state(layer, status);

-- Unified extraction job tracking.
-- Jobs transition: queued → building → complete/failed.
CREATE TABLE IF NOT EXISTS extraction_jobs (
    job_id        TEXT PRIMARY KEY NOT NULL,
    file_id       BLOB NOT NULL,
    unit_id       BLOB,
    layer         TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'queued',
    trigger_query TEXT,
    depends_on    TEXT,
    started_at    TEXT,
    completed_at  TEXT,
    budget_ms     INTEGER,
    error_msg     TEXT,
    FOREIGN KEY (file_id) REFERENCES files(file_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_extraction_jobs_file_layer_status
    ON extraction_jobs(file_id, layer, status);

CREATE INDEX IF NOT EXISTS idx_extraction_jobs_status
    ON extraction_jobs(status);

CREATE UNIQUE INDEX IF NOT EXISTS idx_extraction_jobs_active_file_layer
    ON extraction_jobs(file_id, layer)
    WHERE unit_id IS NULL AND status IN ('queued', 'building');

CREATE UNIQUE INDEX IF NOT EXISTS idx_extraction_jobs_active_unit_layer
    ON extraction_jobs(file_id, unit_id, layer)
    WHERE unit_id IS NOT NULL AND status IN ('queued', 'building');

-- ===== Summary tables (Schema v3) =====

-- Function summary metadata: one row per function.
CREATE TABLE IF NOT EXISTS function_summaries (
    function_id     BLOB PRIMARY KEY NOT NULL,
    node_count      INTEGER NOT NULL,
    edge_count      INTEGER NOT NULL,
    content_hash    TEXT NOT NULL,
    computed_at     TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (function_id) REFERENCES symbols(symbol_id) ON DELETE CASCADE
);

-- Parameter P's downstream reachable targets T ("param → call_arg / return / field").
CREATE TABLE IF NOT EXISTS summary_param_reaches (
    function_id     BLOB NOT NULL,
    param_id        BLOB NOT NULL,
    param_index     INTEGER NOT NULL,
    param_name      TEXT NOT NULL,
    target_kind     TEXT NOT NULL,             -- 'call_arg' | 'return' | 'field'
    target_node_id  BLOB NOT NULL,
    confidence      REAL NOT NULL DEFAULT 0.85,
    provenance      TEXT NOT NULL DEFAULT 'intraprocedural_dataflow',
    FOREIGN KEY (function_id) REFERENCES function_summaries(function_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_spr_function ON summary_param_reaches(function_id);
CREATE INDEX IF NOT EXISTS idx_spr_param   ON summary_param_reaches(param_id);

-- Return node R's upstream sources S ("return ← param / local").
CREATE TABLE IF NOT EXISTS summary_return_sources (
    function_id     BLOB NOT NULL,
    return_id       BLOB NOT NULL,
    source_node_id  BLOB NOT NULL,
    confidence      REAL NOT NULL DEFAULT 0.85,
    provenance      TEXT NOT NULL DEFAULT 'intraprocedural_dataflow',
    FOREIGN KEY (function_id) REFERENCES function_summaries(function_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_srs_function ON summary_return_sources(function_id);
CREATE INDEX IF NOT EXISTS idx_srs_return   ON summary_return_sources(return_id);

-- Call argument A's upstream sources S ("call_arg ← param / local").
CREATE TABLE IF NOT EXISTS summary_call_arg_sources (
    function_id     BLOB NOT NULL,
    callsite_id     BLOB NOT NULL,
    arg_index       INTEGER NOT NULL,
    arg_node_id     BLOB NOT NULL,
    source_node_id  BLOB NOT NULL,
    confidence      REAL NOT NULL DEFAULT 0.85,
    provenance      TEXT NOT NULL DEFAULT 'intraprocedural_dataflow',
    FOREIGN KEY (function_id) REFERENCES function_summaries(function_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_scas_function ON summary_call_arg_sources(function_id);
CREATE INDEX IF NOT EXISTS idx_scas_callsite ON summary_call_arg_sources(callsite_id);

-- FTS5 virtual table for symbol name search
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
    name,
    qualified_name,
    content='symbols',
    content_rowid='rowid'
);

-- domain_rules: language-agnostic domain rule storage.
-- This is NOT a C/C++ ownership subsystem.  It is a generic rule store,
-- match, and learning infrastructure.  All semantics are interpreted by
-- per-language consumers (e.g. CppOwnershipRules for C/C++).
-- **At the core level**: no "ownership", "free", "alloc", "lifecycle".
CREATE TABLE IF NOT EXISTS domain_rules (
    id            TEXT PRIMARY KEY NOT NULL,         -- blake3(lang || 0xff || rule_kind || 0xff || pattern_kind || 0xff || pattern)
    language      TEXT NOT NULL DEFAULT 'c',         -- "c" / "rust" / "python" / "typescript" / "*"
    rule_kind     TEXT NOT NULL,                     -- free_fn / react_hook / ... (language-defined)
    pattern       TEXT NOT NULL,                     -- match target (function name, field path, decorator name, ...)
    pattern_kind  TEXT NOT NULL DEFAULT 'exact',     -- exact / prefix / suffix / glob / regex
    meta          TEXT,                              -- JSON, language-specific extension
    meta_version  INTEGER NOT NULL DEFAULT 1,        -- meta structure version
    source        TEXT NOT NULL,                     -- builtin / learned / user
    status        TEXT NOT NULL DEFAULT 'enabled',   -- candidate / enabled / disabled / rejected / deprecated
    confidence    REAL NOT NULL DEFAULT 1.0,
    created_at    TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Project-level metadata (key-value store for configuration, thresholds, timestamps)
CREATE TABLE IF NOT EXISTS project_metadata (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- User-declared function-pointer dispatch annotations.
-- One annotation maps a struct field (function pointer) to its target function.
CREATE TABLE IF NOT EXISTS function_pointer_annotations (
    annotation_id    TEXT PRIMARY KEY NOT NULL,
    source_symbol    BLOB NOT NULL REFERENCES symbols(symbol_id),
    field_name       TEXT NOT NULL,
    target_symbol    BLOB NOT NULL REFERENCES symbols(symbol_id),
    confidence       REAL NOT NULL DEFAULT 1.0
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_fpa_source_field
    ON function_pointer_annotations(source_symbol, field_name);

CREATE INDEX IF NOT EXISTS idx_fpa_source
    ON function_pointer_annotations(source_symbol);

CREATE INDEX IF NOT EXISTS idx_fpa_target
    ON function_pointer_annotations(target_symbol);

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
        assert!(tables.contains(&"extraction_state".to_string()));
        assert!(tables.contains(&"extraction_jobs".to_string()));
        // Function pointer annotations
        assert!(tables.contains(&"function_pointer_annotations".to_string()));
        // Domain rules
        assert!(tables.contains(&"domain_rules".to_string()));
        // Summary tables
        assert!(tables.contains(&"function_summaries".to_string()));
        assert!(tables.contains(&"summary_param_reaches".to_string()));
        assert!(tables.contains(&"summary_return_sources".to_string()));
        assert!(tables.contains(&"summary_call_arg_sources".to_string()));
    }
}
