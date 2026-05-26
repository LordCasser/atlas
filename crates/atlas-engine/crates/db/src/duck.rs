//! DuckDB backend — high-throughput bulk-write replacement for SQLite.
//!
//! DuckDB's columnar storage and Appender API (bypasses SQL parsing) make
//! bulk inserts 10-100× faster than SQLite for the dense row-per-file
//! patterns of `--analysis full` extraction.
//!
//! ## Usage
//!
//! ```ignore
//! let duck = DuckStore::open_in_memory()?;
//! duck.init_schema()?;
//! duck.write_file_facts(&facts_batch)?;  // 1 × Appender per table
//! ```

use std::path::Path;
use types::*;

use duckdb::{params, Connection};

/// Minimal DuckDB-backed store for bulk writes.
pub struct DuckStore {
    conn: Connection,
}

impl DuckStore {
    // ── Lifecycle ───────────────────────────────────────────────────────

    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "SET threads = 4;
             SET memory_limit = '4GB';
             SET preserve_insertion_order = false;",
        )?;
        Ok(Self { conn })
    }

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

    pub fn init_schema(&self) -> anyhow::Result<()> {
        self.conn.execute_batch(SCHEMA_DDL)?;
        Ok(())
    }

    // ── High-level batch write ──────────────────────────────────────────

    /// Write multiple `FileFacts` in a single pass.  Each table is fed
    /// through its own DuckDB Appender — one Appender open per table,
    /// reused across all files, flushed at the end.
    pub fn write_file_facts(&self, batch: &[FileFacts]) -> anyhow::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }

        let est = batch.len();

        // Accumulate per-table references across all files.
        // with_capacity avoids repeated realloc — 62K files × 50 symbols
        // needs ~22 reallocs without pre-allocation.
        let mut all_files: Vec<&FileInfo> = Vec::with_capacity(est);
        let mut all_symbols: Vec<(&SymbolDef, &str)> = Vec::with_capacity(est * 50);
        let mut all_scopes: Vec<&ScopeDef> = Vec::with_capacity(est * 5);
        let mut all_refs: Vec<&ReferenceUse> = Vec::with_capacity(est * 40);
        let mut all_imports: Vec<&ImportDef> = Vec::with_capacity(est * 5);
        let mut all_edges: Vec<&RawEdge> = Vec::with_capacity(est * 10);
        let mut all_callsites: Vec<&Callsite> = Vec::with_capacity(est * 5);
        let mut all_bindings: Vec<&BindingDef> = Vec::with_capacity(est * 20);
        let mut all_binding_uses: Vec<&BindingUse> = Vec::with_capacity(est * 30);
        let mut all_data_nodes: Vec<&DataNode> = Vec::with_capacity(est * 30);
        let mut all_dataflow_edges: Vec<&DataFlowEdge> = Vec::with_capacity(est * 20);
        let mut all_cfg_nodes: Vec<&CfgNode> = Vec::with_capacity(est * 5);
        let mut all_cfg_edges: Vec<&CfgEdge> = Vec::with_capacity(est * 5);

        for facts in batch {
            all_files.push(&facts.file);
            let layer = facts.layer.as_str();
            for s in &facts.symbols { all_symbols.push((s, layer)); }
            for s in &facts.scopes { all_scopes.push(s); }
            for r in &facts.references { all_refs.push(r); }
            for i in &facts.imports { all_imports.push(i); }
            for e in &facts.raw_edges { all_edges.push(e); }
            for c in &facts.callsites { all_callsites.push(c); }
            for b in &facts.bindings { all_bindings.push(b); }
            for u in &facts.binding_uses { all_binding_uses.push(u); }
            for n in &facts.data_nodes { all_data_nodes.push(n); }
            for e in &facts.dataflow_edges { all_dataflow_edges.push(e); }
            for n in &facts.cfg_nodes { all_cfg_nodes.push(n); }
            for e in &facts.cfg_edges { all_cfg_edges.push(e); }
        }

        let conn = &self.conn;
        write_files_appender(conn, &all_files)?;
        write_symbols_appender(conn, &all_symbols)?;
        write_scopes_appender(conn, &all_scopes)?;
        write_references_appender(conn, &all_refs)?;
        write_imports_appender(conn, &all_imports)?;
        write_edges_appender(conn, &all_edges)?;
        write_callsites_appender(conn, &all_callsites)?;
        write_bindings_appender(conn, &all_bindings)?;
        write_binding_uses_appender(conn, &all_binding_uses)?;
        write_data_nodes_appender(conn, &all_data_nodes)?;
        write_dataflow_edges_appender(conn, &all_dataflow_edges)?;
        write_cfg_nodes_appender(conn, &all_cfg_nodes)?;
        write_cfg_edges_appender(conn, &all_cfg_edges)?;

        Ok(())
    }
}

// ── Schema DDL ─────────────────────────────────────────────────────────────
// PostgreSQL-compatible syntax for DuckDB.  Differences from SQLite:
//   - INTEGER → BIGINT (u64 → signed, DuckDB has no unsigned)
//   - BLOB → BLOB (same)
//   - FOREIGN KEY with ON DELETE CASCADE (DuckDB supports)
//   - datetime('now') → now()
//   - No FTS5 — symbol search stays on SQLite for now

const SCHEMA_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS files (
    file_id       BLOB PRIMARY KEY NOT NULL,
    path          TEXT NOT NULL,
    language      TEXT NOT NULL,
    content_hash  TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'success',
    last_modified TEXT,
    index_time    TIMESTAMP DEFAULT now()
);

CREATE TABLE IF NOT EXISTS symbols (
    symbol_id            BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL,
    kind                 TEXT NOT NULL,
    name                 TEXT NOT NULL,
    qualified_name       TEXT NOT NULL,
    symbol_path_json     TEXT NOT NULL DEFAULT '[]',
    language             TEXT NOT NULL,
    range_start_byte     BIGINT NOT NULL,
    range_end_byte       BIGINT NOT NULL,
    range_start_line     BIGINT NOT NULL,
    range_start_column   BIGINT NOT NULL,
    range_end_line       BIGINT NOT NULL,
    range_end_column     BIGINT NOT NULL,
    name_start_byte      BIGINT NOT NULL,
    name_end_byte        BIGINT NOT NULL,
    name_start_line      BIGINT NOT NULL,
    name_start_column    BIGINT NOT NULL,
    name_end_line        BIGINT NOT NULL,
    name_end_column      BIGINT NOT NULL,
    signature            TEXT,
    visibility           TEXT,
    exported             INTEGER NOT NULL DEFAULT 0,
    static_              INTEGER NOT NULL DEFAULT 0,
    async_               INTEGER NOT NULL DEFAULT 0,
    container_id         BLOB,
    scope_id             BLOB,
    package_name         TEXT,
    namespace_path_json  TEXT NOT NULL DEFAULT '[]',
    layer                TEXT NOT NULL DEFAULT 'structural'
);

CREATE TABLE IF NOT EXISTS scopes (
    scope_id             BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL,
    kind                 TEXT NOT NULL,
    name                 TEXT NOT NULL,
    scope_path           TEXT NOT NULL,
    range_start_byte     BIGINT NOT NULL,
    range_end_byte       BIGINT NOT NULL,
    range_start_line     BIGINT NOT NULL,
    range_start_column   BIGINT NOT NULL,
    range_end_line       BIGINT NOT NULL,
    range_end_column     BIGINT NOT NULL,
    parent_id            BLOB
);

CREATE TABLE IF NOT EXISTS "references" (
    reference_id         BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL,
    source_symbol        BLOB,
    scope_id             BLOB,
    kind                 TEXT NOT NULL,
    text                 TEXT NOT NULL,
    name                 TEXT NOT NULL,
    receiver             TEXT,
    arity                BIGINT,
    range_start_byte     BIGINT NOT NULL,
    range_end_byte       BIGINT NOT NULL,
    range_start_line     BIGINT NOT NULL,
    range_start_column   BIGINT NOT NULL,
    range_end_line       BIGINT NOT NULL,
    range_end_column     BIGINT NOT NULL,
    resolved_symbol_id   BLOB,
    resolved_confidence  REAL,
    resolved_strategy    TEXT,
    resolved_provenance  TEXT,
    binding_id           BLOB
);

CREATE TABLE IF NOT EXISTS imports (
    import_id            BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL,
    kind                 TEXT NOT NULL,
    module               TEXT NOT NULL,
    imported_name        TEXT NOT NULL,
    local_name           TEXT,
    is_wildcard          INTEGER NOT NULL DEFAULT 0,
    is_relative          INTEGER NOT NULL DEFAULT 0,
    range_start_byte     BIGINT NOT NULL,
    range_end_byte       BIGINT NOT NULL,
    range_start_line     BIGINT NOT NULL,
    range_start_column   BIGINT NOT NULL,
    range_end_line       BIGINT NOT NULL,
    range_end_column     BIGINT NOT NULL,
    alias                TEXT
);

CREATE TABLE IF NOT EXISTS symbol_edges (
    edge_id      BLOB PRIMARY KEY NOT NULL,
    source       BLOB NOT NULL,
    target       BLOB,
    kind         TEXT NOT NULL,
    confidence   REAL NOT NULL DEFAULT 0.5,
    provenance   TEXT NOT NULL DEFAULT 'tree_sitter',
    ref_id       BLOB,
    location_0   BIGINT,
    location_1   BIGINT,
    location_2   BIGINT,
    location_3   BIGINT,
    location_4   BIGINT,
    location_5   BIGINT,
    metadata     TEXT,
    resolved_by  TEXT
);

CREATE TABLE IF NOT EXISTS callsites (
    callsite_id          BLOB PRIMARY KEY NOT NULL,
    reference_id         BLOB,
    caller               BLOB NOT NULL,
    callee               BLOB,
    receiver             TEXT,
    args_json            TEXT NOT NULL DEFAULT '[]',
    range_start_byte     BIGINT NOT NULL,
    range_end_byte       BIGINT NOT NULL,
    range_start_line     BIGINT NOT NULL,
    range_start_column   BIGINT NOT NULL,
    range_end_line       BIGINT NOT NULL,
    range_end_column     BIGINT NOT NULL,
    callee_start_line    BIGINT,
    callee_start_column  BIGINT,
    callee_end_line      BIGINT,
    callee_end_column    BIGINT,
    callee_start_byte    BIGINT,
    callee_end_byte      BIGINT
);

CREATE TABLE IF NOT EXISTS bindings (
    binding_id           BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL,
    function_id          BLOB,
    scope_id             BLOB NOT NULL,
    kind                 TEXT NOT NULL,
    name                 TEXT NOT NULL,
    symbol_id            BLOB,
    range_start_byte     BIGINT NOT NULL,
    range_end_byte       BIGINT NOT NULL,
    range_start_line     BIGINT NOT NULL,
    range_start_column   BIGINT NOT NULL,
    range_end_line       BIGINT NOT NULL,
    range_end_column     BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS binding_uses (
    binding_use_id       BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL,
    scope_id             BLOB,
    binding_id           BLOB,
    reference_id         BLOB,
    name                 TEXT NOT NULL,
    range_start_byte     BIGINT NOT NULL,
    range_end_byte       BIGINT NOT NULL,
    range_start_line     BIGINT NOT NULL,
    range_start_column   BIGINT NOT NULL,
    range_end_line       BIGINT NOT NULL,
    range_end_column     BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS data_nodes (
    data_node_id         BLOB PRIMARY KEY NOT NULL,
    file_id              BLOB NOT NULL,
    function_id          BLOB,
    kind                 TEXT NOT NULL,
    binding_id           BLOB,
    callsite_id          BLOB,
    name                 TEXT,
    access_path          TEXT,
    arg_index            BIGINT,
    range_start_byte     BIGINT NOT NULL,
    range_end_byte       BIGINT NOT NULL,
    range_start_line     BIGINT NOT NULL,
    range_start_column   BIGINT NOT NULL,
    range_end_line       BIGINT NOT NULL,
    range_end_column     BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS dataflow_edges (
    dataflow_edge_id     BLOB PRIMARY KEY NOT NULL,
    source               BLOB NOT NULL,
    target               BLOB NOT NULL,
    kind                 TEXT NOT NULL,
    location_0           BIGINT,
    location_1           BIGINT,
    location_2           BIGINT,
    location_3           BIGINT,
    location_4           BIGINT,
    location_5           BIGINT,
    confidence           REAL NOT NULL DEFAULT 0.8
);

CREATE TABLE IF NOT EXISTS cfg_nodes (
    cfg_node_id          BLOB PRIMARY KEY NOT NULL,
    function_id          BLOB NOT NULL,
    kind                 TEXT NOT NULL,
    range_start_byte     BIGINT NOT NULL,
    range_end_byte       BIGINT NOT NULL,
    range_start_line     BIGINT NOT NULL,
    range_start_column   BIGINT NOT NULL,
    range_end_line       BIGINT NOT NULL,
    range_end_column     BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS cfg_edges (
    cfg_edge_id          BLOB PRIMARY KEY NOT NULL,
    source               BLOB NOT NULL,
    target               BLOB NOT NULL,
    kind                 TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS project_metadata (
    key         TEXT PRIMARY KEY NOT NULL,
    value       TEXT NOT NULL,
    updated_at  TIMESTAMP DEFAULT now()
);

CREATE TABLE IF NOT EXISTS schema_versions (
    version     BIGINT PRIMARY KEY,
    applied_at  TIMESTAMP DEFAULT now(),
    description TEXT
);
"#;

// ── Bulk insert helpers (Appender-based) ───────────────────────────────────

fn blob(id: &impl BlobId) -> &[u8] { id.as_bytes().as_slice() }
fn opt_blob(id: &Option<impl BlobId>) -> Option<&[u8]> {
    id.as_ref().map(|i| i.as_bytes().as_slice())
}

trait BlobId { fn as_bytes(&self) -> &[u8; 32]; }
impl BlobId for SymbolId { fn as_bytes(&self) -> &[u8; 32] { self.as_bytes() } }
impl BlobId for FileId { fn as_bytes(&self) -> &[u8; 32] { self.as_bytes() } }
impl BlobId for ScopeId { fn as_bytes(&self) -> &[u8; 32] { self.as_bytes() } }
impl BlobId for ReferenceId { fn as_bytes(&self) -> &[u8; 32] { self.as_bytes() } }
impl BlobId for ImportId { fn as_bytes(&self) -> &[u8; 32] { self.as_bytes() } }
impl BlobId for EdgeId { fn as_bytes(&self) -> &[u8; 32] { self.as_bytes() } }
impl BlobId for CallsiteId { fn as_bytes(&self) -> &[u8; 32] { self.as_bytes() } }
impl BlobId for BindingId { fn as_bytes(&self) -> &[u8; 32] { self.as_bytes() } }
impl BlobId for BindingUseId { fn as_bytes(&self) -> &[u8; 32] { self.as_bytes() } }
impl BlobId for DataNodeId { fn as_bytes(&self) -> &[u8; 32] { self.as_bytes() } }
impl BlobId for DataFlowEdgeId { fn as_bytes(&self) -> &[u8; 32] { self.as_bytes() } }
impl BlobId for CfgNodeId { fn as_bytes(&self) -> &[u8; 32] { self.as_bytes() } }
impl BlobId for CfgEdgeId { fn as_bytes(&self) -> &[u8; 32] { self.as_bytes() } }

fn write_files_appender(conn: &Connection, files: &[&FileInfo]) -> anyhow::Result<()> {
    if files.is_empty() { return Ok(()); }
    let mut app = conn.appender("files")?;
    for fi in files {
        app.append_row(params![
            blob(&fi.file_id),
            fi.path.as_str(),
            fi.language.as_str(),
            fi.content_hash.as_str(),
            fi.status.as_str(),
        ])?;
    }
    Ok(())
}

fn write_symbols_appender(conn: &Connection, symbols: &[(&SymbolDef, &str)]) -> anyhow::Result<()> {
    if symbols.is_empty() { return Ok(()); }
    let mut app = conn.appender("symbols")?;
    for (s, layer) in symbols {
        let path_json = serde_json::to_string(&s.symbol_path).unwrap_or_else(|_| "[]".into());
        let ns_json = serde_json::to_string(&s.namespace_path).unwrap_or_else(|_| "[]".into());
        app.append_row(params![
            blob(&s.id), blob(&s.file_id), s.kind.as_str(), s.name.as_str(),
            s.qualified_name.as_str(), path_json.as_str(), s.language.as_str(),
            s.range.start_byte as i64, s.range.end_byte as i64,
            s.range.start_line as i64, s.range.start_column as i64,
            s.range.end_line as i64, s.range.end_column as i64,
            s.name_range.start_byte as i64, s.name_range.end_byte as i64,
            s.name_range.start_line as i64, s.name_range.start_column as i64,
            s.name_range.end_line as i64, s.name_range.end_column as i64,
            s.signature.as_deref().unwrap_or(""),
            s.visibility.as_ref().map(|v| v.as_str()).unwrap_or(""),
            s.exported as i32, s.static_ as i32, s.async_ as i32,
            opt_blob(&s.container), opt_blob(&s.scope_id),
            s.package_name.as_deref().unwrap_or(""), ns_json.as_str(),
            layer,
        ])?;
    }
    Ok(())
}

fn write_scopes_appender(conn: &Connection, scopes: &[&ScopeDef]) -> anyhow::Result<()> {
    if scopes.is_empty() { return Ok(()); }
    let mut app = conn.appender("scopes")?;
    for s in scopes {
        app.append_row(params![
            blob(&s.id), blob(&s.file_id), s.kind.as_str(), s.name.as_str(),
            s.scope_path.as_str(),
            s.range.start_byte as i64, s.range.end_byte as i64,
            s.range.start_line as i64, s.range.start_column as i64,
            s.range.end_line as i64, s.range.end_column as i64,
            opt_blob(&s.parent_id),
        ])?;
    }
    Ok(())
}

fn write_references_appender(conn: &Connection, refs: &[&ReferenceUse]) -> anyhow::Result<()> {
    if refs.is_empty() { return Ok(()); }
    let mut app = conn.appender("\"references\"")?;
    for r in refs {
        app.append_row(params![
            blob(&r.id), blob(&r.file_id), opt_blob(&r.source_symbol),
            opt_blob(&r.scope_id), r.kind.as_str(), r.text.as_str(),
            r.name.as_str(), r.receiver.as_deref().unwrap_or(""),
            r.arity.map(|a| a as i64),
            r.range.start_byte as i64, r.range.end_byte as i64,
            r.range.start_line as i64, r.range.start_column as i64,
            r.range.end_line as i64, r.range.end_column as i64,
            Option::<&[u8]>::None, Option::<f64>::None,
            Option::<&str>::None, Option::<&str>::None,
            Option::<&[u8]>::None,
        ])?;
    }
    Ok(())
}

fn write_imports_appender(conn: &Connection, imports: &[&ImportDef]) -> anyhow::Result<()> {
    if imports.is_empty() { return Ok(()); }
    let mut app = conn.appender("imports")?;
    for imp in imports {
        app.append_row(params![
            blob(&imp.id), blob(&imp.file_id), imp.kind.as_str(),
            imp.module.as_str(), imp.imported_name.as_str(),
            imp.local_name.as_deref().unwrap_or(""),
            imp.is_wildcard as i32, imp.is_relative as i32,
            imp.range.start_byte as i64, imp.range.end_byte as i64,
            imp.range.start_line as i64, imp.range.start_column as i64,
            imp.range.end_line as i64, imp.range.end_column as i64,
            imp.alias.as_deref().unwrap_or(""),
        ])?;
    }
    Ok(())
}

fn write_edges_appender(conn: &Connection, edges: &[&RawEdge]) -> anyhow::Result<()> {
    if edges.is_empty() { return Ok(()); }
    let mut app = conn.appender("symbol_edges")?;
    for e in edges {
        let loc = e.location.as_ref();
        app.append_row(params![
            blob(&e.id), blob(&e.source), blob(&e.target),
            e.kind.as_str(), e.confidence.as_f32(),
            e.provenance.as_str(), opt_blob(&e.ref_id),
            loc.map(|r| r.start_byte as i64), loc.map(|r| r.end_byte as i64),
            loc.map(|r| r.start_line as i64), loc.map(|r| r.start_column as i64),
            loc.map(|r| r.end_line as i64), loc.map(|r| r.end_column as i64),
            e.metadata.as_deref().unwrap_or(""),
            Option::<&str>::None,
        ])?;
    }
    Ok(())
}

fn write_callsites_appender(conn: &Connection, callsites: &[&Callsite]) -> anyhow::Result<()> {
    if callsites.is_empty() { return Ok(()); }
    let mut app = conn.appender("callsites")?;
    for c in callsites {
        let args_json = serde_json::to_string(&c.args).unwrap_or_else(|_| "[]".into());
        let cr = c.callee_range.as_ref();
        app.append_row(params![
            blob(&c.id), opt_blob(&c.reference_id),
            blob(&c.caller), opt_blob(&c.callee),
            c.receiver.as_deref().unwrap_or(""),
            args_json.as_str(),
            c.range.start_byte as i64, c.range.end_byte as i64,
            c.range.start_line as i64, c.range.start_column as i64,
            c.range.end_line as i64, c.range.end_column as i64,
            cr.map(|r| r.start_line as i64),
            cr.map(|r| r.start_column as i64),
            cr.map(|r| r.end_line as i64),
            cr.map(|r| r.end_column as i64),
            cr.map(|r| r.start_byte as i64),
            cr.map(|r| r.end_byte as i64),
        ])?;
    }
    Ok(())
}

fn write_bindings_appender(conn: &Connection, bindings: &[&BindingDef]) -> anyhow::Result<()> {
    if bindings.is_empty() { return Ok(()); }
    let mut app = conn.appender("bindings")?;
    for b in bindings {
        app.append_row(params![
            blob(&b.id), blob(&b.file_id), opt_blob(&b.function_id),
            blob(&b.scope_id), b.kind.as_str(), b.name.as_str(),
            opt_blob(&b.symbol_id),
            b.range.start_byte as i64, b.range.end_byte as i64,
            b.range.start_line as i64, b.range.start_column as i64,
            b.range.end_line as i64, b.range.end_column as i64,
        ])?;
    }
    Ok(())
}

fn write_binding_uses_appender(conn: &Connection, uses: &[&BindingUse]) -> anyhow::Result<()> {
    if uses.is_empty() { return Ok(()); }
    let mut app = conn.appender("binding_uses")?;
    for u in uses {
        app.append_row(params![
            blob(&u.id), blob(&u.file_id), blob(&u.scope_id),
            opt_blob(&u.binding_id), opt_blob(&u.reference_id),
            u.name.as_str(),
            u.range.start_byte as i64, u.range.end_byte as i64,
            u.range.start_line as i64, u.range.start_column as i64,
            u.range.end_line as i64, u.range.end_column as i64,
        ])?;
    }
    Ok(())
}

fn write_data_nodes_appender(conn: &Connection, nodes: &[&DataNode]) -> anyhow::Result<()> {
    if nodes.is_empty() { return Ok(()); }
    let mut app = conn.appender("data_nodes")?;
    for n in nodes {
        app.append_row(params![
            blob(&n.id), blob(&n.file_id), opt_blob(&n.function_id),
            n.kind.as_str(), opt_blob(&n.binding_id),
            opt_blob(&n.callsite_id), n.name.as_deref().unwrap_or(""),
            n.access_path.as_deref().unwrap_or(""),
            n.arg_index.map(|v| v as i64),
            n.range.start_byte as i64, n.range.end_byte as i64,
            n.range.start_line as i64, n.range.start_column as i64,
            n.range.end_line as i64, n.range.end_column as i64,
        ])?;
    }
    Ok(())
}

fn write_dataflow_edges_appender(conn: &Connection, edges: &[&DataFlowEdge]) -> anyhow::Result<()> {
    if edges.is_empty() { return Ok(()); }
    let mut app = conn.appender("dataflow_edges")?;
    for e in edges {
        app.append_row(params![
            blob(&e.id), blob(&e.source), blob(&e.target), e.kind.as_str(),
            e.location.start_byte as i64, e.location.end_byte as i64,
            e.location.start_line as i64, e.location.start_column as i64,
            e.location.end_line as i64, e.location.end_column as i64,
            e.confidence,
        ])?;
    }
    Ok(())
}

fn write_cfg_nodes_appender(conn: &Connection, nodes: &[&CfgNode]) -> anyhow::Result<()> {
    if nodes.is_empty() { return Ok(()); }
    let mut app = conn.appender("cfg_nodes")?;
    for n in nodes {
        app.append_row(params![
            blob(&n.id), blob(&n.function_id), n.kind.as_str(),
            n.stmt_range.start_byte as i64, n.stmt_range.end_byte as i64,
            n.stmt_range.start_line as i64, n.stmt_range.start_column as i64,
            n.stmt_range.end_line as i64, n.stmt_range.end_column as i64,
        ])?;
    }
    Ok(())
}

fn write_cfg_edges_appender(conn: &Connection, edges: &[&CfgEdge]) -> anyhow::Result<()> {
    if edges.is_empty() { return Ok(()); }
    let mut app = conn.appender("cfg_edges")?;
    for e in edges {
        app.append_row(params![
            blob(&e.id), blob(&e.source), blob(&e.target), e.kind.as_str(),
        ])?;
    }
    Ok(())
}
