//! Atlas Store — the single SQLite persistence layer.
//!
//! `Store` wraps a `Mutex<Connection>` and provides all CRUD operations.
//! Implementation is split across domain submodules:
//!
//! | Module | Domain |
//! |--------|--------|
//! | `lifecycle` | DB open, schema init, cross-process locking |
//! | `files`     | File CRUD |
//! | `symbols`   | Symbol CRUD, search, bulk queries |
//! | `edges`     | References, edges, callsites, invalidation |
//! | `scopes`    | Scopes + imports |
//! | `dataflow`  | Bindings, data nodes, dataflow edges |
//! | `cfg`       | Control-flow graph |
//! | `stats`     | Metadata, stats, path resolution |
//!
//! ## Reader / Writer trait split
//!
//! Four reader traits (`SymbolReader`, `DataflowReader`, `CallGraphReader`,
//! `FileReader`) are defined in [`crate::readers`].  Consumer code should
//! accept `impl SymbolReader + ...` instead of `&Store` to reduce coupling.
//! Writer traits are deferred to a future cleanup pass (Item 10).

use rusqlite::{Connection, Transaction, params};
use std::collections::HashSet;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use types::*;

use crate::store_rows::*;
use crate::store_writers::*;

mod artifacts;
mod cfg;
mod dataflow;
mod edges;
mod files;
mod index_layers;
mod lifecycle;
mod scopes;
mod stats;
mod symbols;

// ---------------------------------------------------------------------------
// StoreReader — read-only query interface
// ---------------------------------------------------------------------------

/// Read-only query interface backed by a dedicated SQLite read connection.
///
/// All methods take `&self` and perform only SELECT queries on a separate
/// connection opened in `query_only` mode.  This allows concurrent reads
/// during write transactions (WAL mode), avoiding the single-connection
/// bottleneck.
///
/// For mutations, use `Store` which owns the write connection and derefs
/// to `StoreReader`.
pub struct StoreReader {
    /// Write connection — all INSERT/UPDATE/DELETE/reference resolution.
    pub(crate) conn: Mutex<Connection>,
    /// Read connection — all SELECT queries.  For file-backed databases
    /// this is a second `Connection` opened with `PRAGMA query_only = ON`
    /// so reads never block writes (WAL mode).  For in-memory databases
    /// this is `None` and reads fall back to the write connection.
    pub(crate) read_conn: Option<Mutex<Connection>>,
}

impl StoreReader {
    /// Lock the read connection for SELECT queries.
    ///
    /// For file-backed databases the read connection uses `PRAGMA query_only = ON`
    /// and runs independently from the write connection.  For in-memory databases
    /// this falls back to the write connection.
    fn lock_read(&self) -> std::sync::MutexGuard<'_, Connection> {
        match &self.read_conn {
            Some(rc) => rc.lock().unwrap_or_else(|e| e.into_inner()),
            None => self.conn.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }

    /// Find a symbol by its deterministic SymbolId.
    pub fn find_symbol_by_id(&self, id: &SymbolId) -> anyhow::Result<Option<SymbolDef>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json, layer
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
        let conn = self.lock_read();
        let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json, layer
             FROM symbols WHERE symbol_id IN ({})",
            placeholders.join(","),
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
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
    db_path: PathBuf,
}

impl Store {
    /// Return the SQLite database file path (or `":memory:"` for in-memory stores).
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
}

impl Deref for Store {
    type Target = StoreReader;

    fn deref(&self) -> &Self::Target {
        &self.reader
    }
}

impl Store {
    // -----------------------------------------------------------------------
    // Internal helpers (accessed by domain submodules via module privacy)
    // -----------------------------------------------------------------------

    /// Lock the write connection for mutations.
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.reader.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Lock the read connection for SELECT queries.
    ///
    /// The read connection uses `PRAGMA query_only = ON` and runs
    /// independently from the write connection.  In WAL mode this
    /// allows concurrent reads during write transactions.
    pub(crate) fn lock_read(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.reader.lock_read()
    }

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
                write_symbols(&tx, &facts.symbols, &facts.layer)?;
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
                    .filter(|callsite| {
                        valid_sources.contains(&callsite.caller)
                            && callsite
                                .callee
                                .map_or(true, |callee| valid_sources.contains(&callee))
                    })
                    .cloned()
                    .collect();
                if !valid_callsites.is_empty() {
                    write_callsites(&tx, &valid_callsites)?;
                }
            }

            // Binding data — FK guarded (scope_id, function_id for bindings;
            // binding_id, scope_id for uses)
            let valid_bindings: Vec<_> = facts
                .bindings
                .iter()
                .filter(|b| {
                    b.function_id
                        .map_or(true, |fid| valid_sources.contains(&fid))
                        && facts.scopes.iter().any(|s| s.id == b.scope_id)
                        && b.symbol_id.map_or(true, |sid| valid_sources.contains(&sid))
                })
                .cloned()
                .collect();
            if !valid_bindings.is_empty() {
                write_bindings(&tx, &valid_bindings)?;
            }
            let valid_binding_ids: HashSet<_> = valid_bindings.iter().map(|b| b.id).collect();
            if !facts.binding_uses.is_empty() {
                let valid_uses: Vec<_> = facts
                    .binding_uses
                    .iter()
                    .filter(|bu| {
                        bu.binding_id
                            .map_or(false, |bid| valid_binding_ids.contains(&bid))
                            && facts.scopes.iter().any(|s| s.id == bu.scope_id)
                    })
                    .cloned()
                    .collect();
                if !valid_uses.is_empty() {
                    write_binding_uses(&tx, &valid_uses)?;
                }
            }

            // Dataflow + CFG data — FK guarded like edges/callsites above
            if !facts.data_nodes.is_empty() {
                // Filter out data_nodes whose function_id references a symbol
                // not in this batch (cross-file reference).  These will be
                // resolved on a subsequent pass once the target symbol exists.
                let safe_nodes: Vec<_> = facts
                    .data_nodes
                    .iter()
                    .filter(|dn| {
                        dn.function_id
                            .map_or(true, |fid| valid_sources.contains(&fid))
                            && dn
                                .binding_id
                                .map_or(true, |bid| valid_binding_ids.contains(&bid))
                    })
                    .cloned()
                    .collect();
                if !safe_nodes.is_empty() {
                    write_data_nodes(&tx, &safe_nodes)?;
                }
            }
            if !facts.dataflow_edges.is_empty() {
                // Guard: only write edges whose source and target exist among
                // the data_nodes we just committed (or will commit).
                let valid_node_ids: HashSet<_> = facts
                    .data_nodes
                    .iter()
                    .filter(|dn| {
                        dn.function_id
                            .map_or(true, |fid| valid_sources.contains(&fid))
                            && dn
                                .binding_id
                                .map_or(true, |bid| valid_binding_ids.contains(&bid))
                    })
                    .map(|dn| dn.id)
                    .collect();
                let safe_edges: Vec<_> = facts
                    .dataflow_edges
                    .iter()
                    .filter(|e| {
                        valid_node_ids.contains(&e.source) && valid_node_ids.contains(&e.target)
                    })
                    .cloned()
                    .collect();
                if !safe_edges.is_empty() {
                    write_dataflow_edges(&tx, &safe_edges)?;
                }
            }
            if !facts.cfg_nodes.is_empty() {
                let safe_cfg: Vec<_> = facts
                    .cfg_nodes
                    .iter()
                    .filter(|cn| valid_sources.contains(&cn.function_id))
                    .cloned()
                    .collect();
                if !safe_cfg.is_empty() {
                    write_cfg_nodes(&tx, &safe_cfg)?;
                }
            }
            if !facts.cfg_edges.is_empty() {
                let valid_cfg_ids: HashSet<_> = facts
                    .cfg_nodes
                    .iter()
                    .filter(|cn| valid_sources.contains(&cn.function_id))
                    .map(|cn| cn.id)
                    .collect();
                let safe_cfg_edges: Vec<_> = facts
                    .cfg_edges
                    .iter()
                    .filter(|e| {
                        valid_cfg_ids.contains(&e.source) && valid_cfg_ids.contains(&e.target)
                    })
                    .cloned()
                    .collect();
                if !safe_cfg_edges.is_empty() {
                    write_cfg_edges(&tx, &safe_cfg_edges)?;
                }
            }

            // Record per-file per-layer index status.
            // INSERT OR REPLACE semantics: overwrites existing row on
            // (file_id, layer) conflict — no audit trail needed here.
            let status = if facts.budget_exceeded {
                "partial"
            } else {
                "complete"
            };
            tx.execute(
                "INSERT OR REPLACE INTO file_index_layers
                    (file_id, layer, content_hash, status, updated_at)
                 VALUES (?1, ?2, ?3, ?4, datetime('now'))",
                params![
                    facts.file.file_id,
                    facts.layer,
                    facts.file.content_hash,
                    status
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
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
// FileReader) are defined in [crate::readers] and implemented on Store
// below.  Each delegates via UFCS to the store's inherent method.
//
// trace/analysis code can accept `impl SymbolReader + DataflowReader +
// CallGraphReader + FileReader` instead of `&Store` for layered access.

use crate::readers::*;

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
    fn search_symbols_with_limit(
        &self,
        query: &str,
        limit: usize,
        kind_filter: Option<&SymbolKind>,
    ) -> anyhow::Result<Vec<SymbolDef>> {
        Store::search_symbols_with_limit(self, query, limit, kind_filter)
    }
    fn search_symbols_by_name_like(
        &self,
        pattern: &str,
        language: Option<&Language>,
        limit: usize,
        kind_filter: Option<&SymbolKind>,
    ) -> anyhow::Result<Vec<SymbolDef>> {
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
    fn get_data_nodes(
        &self,
        ids: &[DataNodeId],
    ) -> anyhow::Result<std::collections::HashMap<DataNodeId, DataNode>> {
        Store::get_data_nodes(self, ids)
    }
    fn find_data_nodes_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<DataNode>> {
        Store::find_data_nodes_by_file(self, file_id)
    }
    fn find_data_nodes_by_function(&self, function_id: &SymbolId) -> anyhow::Result<Vec<DataNode>> {
        Store::find_data_nodes_by_function(self, function_id)
    }
    fn find_data_nodes_by_callsite(
        &self,
        callsite_id: &CallsiteId,
    ) -> anyhow::Result<Vec<DataNode>> {
        Store::find_data_nodes_by_callsite(self, callsite_id)
    }
    fn find_dataflow_edges_by_source(
        &self,
        source: &DataNodeId,
    ) -> anyhow::Result<Vec<DataFlowEdge>> {
        Store::find_dataflow_edges_by_source(self, source)
    }
    fn find_dataflow_edges_by_target(
        &self,
        target: &DataNodeId,
    ) -> anyhow::Result<Vec<DataFlowEdge>> {
        Store::find_dataflow_edges_by_target(self, target)
    }
    fn find_dataflow_edges_by_sources(
        &self,
        sources: &[DataNodeId],
    ) -> anyhow::Result<Vec<DataFlowEdge>> {
        Store::find_dataflow_edges_by_sources(self, sources)
    }
    fn find_dataflow_edges_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<DataFlowEdge>> {
        Store::find_dataflow_edges_by_file(self, file_id)
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
    fn find_callsite_by_reference_id(
        &self,
        reference_id: &ReferenceId,
    ) -> anyhow::Result<Option<Callsite>> {
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
    fn find_binding_uses_by_binding(
        &self,
        binding_id: &BindingId,
    ) -> anyhow::Result<Vec<BindingUse>> {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
            layer: "structural".to_string(),
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
    fn insert_file_facts_handles_child_before_parent_order() {
        let store = test_store();
        let file_id = FileId::generate("src/nested.c");
        let range = TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 11,
        };

        let parent_scope_id = ScopeId::generate(&file_id, None, ScopeKind::Function.as_str(), 0);
        let child_scope_id = ScopeId::generate(
            &file_id,
            Some(&parent_scope_id),
            ScopeKind::Block.as_str(),
            5,
        );
        let child_scope = ScopeDef {
            id: child_scope_id,
            file_id,
            kind: ScopeKind::Block,
            name: "block".into(),
            scope_path: "parent:block".into(),
            range,
            parent_id: Some(parent_scope_id),
        };
        let parent_scope = ScopeDef {
            id: parent_scope_id,
            file_id,
            kind: ScopeKind::Function,
            name: "parent".into(),
            scope_path: "parent".into(),
            range,
            parent_id: None,
        };

        let parent = test_symbol(file_id, "parent", SymbolKind::Function);
        let mut child = test_symbol(file_id, "child", SymbolKind::Function);
        child.container = Some(parent.id);

        let facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "src/nested.c".into(),
                language: Language::C,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            },
            symbols: vec![child.clone(), parent.clone()],
            scopes: vec![child_scope, parent_scope],
            ..Default::default()
        };

        store.insert_file_facts(&facts).unwrap();

        let found_child = store.find_symbol_by_id(&child.id).unwrap().unwrap();
        assert_eq!(found_child.container, Some(parent.id));
        let scopes = store.find_scopes_by_file(&file_id).unwrap();
        let found_child_scope = scopes.iter().find(|s| s.id == child_scope_id).unwrap();
        assert_eq!(found_child_scope.parent_id, Some(parent_scope_id));
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

    /// Regression (Item 10): `Store::open_db` is a pure primitive — it does NOT
    /// create parent directories. Callers (typically workspace layer) must ensure
    /// the directory exists before calling.
    #[test]
    fn store_open_db_does_not_create_parent_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("nonexistent").join("atlas.db");
        // Parent dir does not exist — open_db should fail with rusqlite error
        assert!(Store::open_db(&db_path).is_err());
        // And it must NOT have created the parent dir
        assert!(!db_path.parent().unwrap().exists());
    }

    /// `Store::open_db` works at an explicit file path when parent dir exists.
    #[test]
    fn store_open_db_works_with_existing_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        let store = Store::open_db(&db_path).unwrap();
        store.init_schema().unwrap();
        let stats = store.get_stats().unwrap();
        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.sqlite_version.len() > 0, true);
    }
}
