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
//! | `summary`   | Function summaries (persistence + query) |
//! | `stats`     | Metadata, stats, path resolution |
//! | `annotations` | Function-pointer dispatch annotations |
//! | `extraction_state` | Extraction state tracking |
//! | `extraction_jobs` | Extraction job tracking (queued/building/complete/failed) |
//!
//! ## Reader / Writer trait split
//!
//! Four reader traits (`SymbolReader`, `DataflowReader`, `CallGraphReader`,
//! `FileReader`) are defined in [`crate::readers`].  Consumer code should
//! accept `impl SymbolReader + ...` instead of `&Store` to reduce coupling.
//! Writer traits are deferred to a future cleanup pass (Item 10).

use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use types::*;

use crate::store_rows::*;
use crate::store_writers::*;

mod annotations;
mod cfg;
mod closure_coverage;
mod closure_generations;
mod dataflow;
pub(crate) mod domain_rules;
mod edges;
pub(crate) mod extraction_jobs;
mod extraction_state;
pub mod file_inventory;
mod files;
mod fk_guards;
mod lifecycle;
pub(crate) mod reference_resolutions;
pub mod symbol_edge_candidates;
#[allow(unused_imports)]
pub use lifecycle::{
    ExclusiveLockHeld, KEY_GRAPH_GENERATION, KEY_RESOLUTION_CONFIG_HASH, KEY_RESOLUTION_GENERATION,
    PipelineGrade,
};
mod scopes;
mod stats;
pub mod summary;
pub mod symbol_hints;
mod symbols;

// ---------------------------------------------------------------------------
// WalCheckpointStats — WAL checkpoint result statistics
// ---------------------------------------------------------------------------

/// WAL checkpoint result statistics.
#[derive(Debug, Default, Clone)]
pub struct WalCheckpointStats {
    pub busy: i64,
    pub log_frames: i64,
    pub checkpointed_frames: i64,
    pub elapsed_ms: u128,
}

// ---------------------------------------------------------------------------
// StoreReader — read-only query interface
// ---------------------------------------------------------------------------

/// Total read-side page cache budget in KiB, split across the read pool.
///
/// Matches the single-connection budget the pool replaced, so adding
/// connections buys parallelism without multiplying memory.
pub(crate) const READ_CACHE_BUDGET_KIB: usize = 65536;

/// Number of SQLite read connections opened for file-backed databases.
///
/// Sized to the available parallelism so every rayon worker can usually hold
/// its own connection.  Clamped because each connection carries its own page
/// cache and file descriptor, so an unbounded pool would multiply both.
pub(crate) fn read_pool_size() -> usize {
    std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .clamp(2, 8)
}

/// Stable per-thread hint used to pick a read-pool slot.
fn read_slot_hint() -> usize {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static SLOT: Cell<Option<usize>> = const { Cell::new(None) };
    }
    SLOT.with(|slot| match slot.get() {
        Some(v) => v,
        None => {
            let v = NEXT.fetch_add(1, Ordering::Relaxed);
            slot.set(Some(v));
            v
        }
    })
}

/// Read-only query interface backed by a pool of dedicated SQLite read
/// connections.
///
/// All methods take `&self` and perform only SELECT queries on separate
/// connections opened in `query_only` mode.  This allows concurrent reads
/// during write transactions (WAL mode) and lets parallel readers run
/// without serializing behind a single connection.
///
/// For mutations, use `Store` which owns the write connection and derefs
/// to `StoreReader`.
pub struct StoreReader {
    /// Write connection — all INSERT/UPDATE/DELETE/reference resolution.
    pub(crate) conn: Mutex<Connection>,
    /// Read connection pool — all SELECT queries.  For file-backed databases
    /// this holds several `Connection`s opened with `PRAGMA query_only = ON`
    /// so reads never block writes (WAL mode) *and* parallel readers do not
    /// serialize behind a single mutex.  For in-memory databases this is
    /// empty and reads fall back to the write connection.
    ///
    /// See `docs/performance.md` Methodology §17 — a single read connection
    /// turns every rayon worker into a queue behind one mutex.
    pub(crate) read_pool: Vec<Mutex<Connection>>,
}

impl StoreReader {
    /// Lock a read connection for SELECT queries.
    ///
    /// For file-backed databases the read connections use `PRAGMA query_only = ON`
    /// and run independently from the write connection.  For in-memory databases
    /// this falls back to the write connection.
    ///
    /// Slot selection is thread-affine (each thread keeps the slot it was first
    /// assigned) so a rayon worker reuses the same connection — preserving
    /// SQLite's per-connection prepared-statement and page caches.  If that slot
    /// is busy the remaining slots are probed with `try_lock` before blocking.
    ///
    /// Callers must still never hold a read guard across another read: with a
    /// pool that usually finds a free slot instead of deadlocking, so it would
    /// fail only under contention.  In-memory databases fall through to the
    /// single write connection and deadlock deterministically, which is what
    /// the tests exercise.
    fn lock_read(&self) -> std::sync::MutexGuard<'_, Connection> {
        let n = self.read_pool.len();
        if n == 0 {
            return self.conn.lock().unwrap_or_else(|e| e.into_inner());
        }
        let home = read_slot_hint() % n;
        if let Ok(guard) = self.read_pool[home].try_lock() {
            return guard;
        }
        for offset in 1..n {
            if let Ok(guard) = self.read_pool[(home + offset) % n].try_lock() {
                return guard;
            }
        }
        self.read_pool[home]
            .lock()
            .unwrap_or_else(|e| e.into_inner())
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

    /// Find the file_id for a symbol (lightweight — only queries one column).
    ///
    /// Returns `Ok(Some(file_id))` when the symbol exists, `Ok(None)` when
    /// the symbol is not found, or `Err(...)` on database errors.
    pub fn find_symbol_file(&self, symbol_id: &SymbolId) -> anyhow::Result<Option<FileId>> {
        let conn = self.lock_read();
        let result: Result<Option<FileId>, _> = conn.query_row(
            "SELECT file_id FROM symbols WHERE symbol_id = ?1",
            params![symbol_id],
            |row| row.get(0),
        );
        match result {
            Ok(file_id) => Ok(file_id),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
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

    /// Trigger a PASSIVE WAL checkpoint and return statistics.
    ///
    /// Under heavy writes (e.g. bulk indexing), the WAL can grow without
    /// bound and each subsequent transaction incurs O(WAL-size) overhead.
    /// Calling this periodically keeps the WAL small and write throughput
    /// steady.
    ///
    /// PASSIVE mode does not block concurrent writers — it checkpoints as
    /// much as it can without interfering.  Callers that want a hard flush
    /// after the write phase should use `checkpoint_wal_truncate`.
    pub fn checkpoint_wal(&self) -> anyhow::Result<WalCheckpointStats> {
        let conn = self.lock();
        let started = Instant::now();
        let mut stmt = conn.prepare("PRAGMA wal_checkpoint(PASSIVE);")?;
        let (busy, log_frames, checkpointed_frames): (i64, i64, i64) =
            stmt.query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        let elapsed = started.elapsed();
        Ok(WalCheckpointStats {
            busy,
            log_frames,
            checkpointed_frames,
            elapsed_ms: elapsed.as_millis(),
        })
    }

    /// Force a full WAL checkpoint and truncate the WAL file to zero bytes.
    /// Blocks writers.  Use at the end of a bulk write phase.
    pub fn checkpoint_wal_truncate(&self) -> anyhow::Result<WalCheckpointStats> {
        let conn = self.lock();
        let started = Instant::now();
        let mut stmt = conn.prepare("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let (busy, log_frames, checkpointed_frames): (i64, i64, i64) =
            stmt.query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        let elapsed = started.elapsed();
        Ok(WalCheckpointStats {
            busy,
            log_frames,
            checkpointed_frames,
            elapsed_ms: elapsed.as_millis(),
        })
    }

    /// Enter bulk-write mode for maximum throughput during rebuilds.
    ///
    /// Returns a [`BulkWriteGuard`] that restores safety defaults on drop,
    /// even on panic.
    ///
    /// **The database may be corrupted on power loss or crash while
    /// bulk-write mode is active.**  Only use during index rebuilds where
    /// the data can be regenerated.
    pub fn enter_bulk_write(&self) -> anyhow::Result<BulkWriteGuard<'_>> {
        let conn = self.lock();
        conn.execute_batch(
            "PRAGMA synchronous = OFF;
             PRAGMA foreign_keys = OFF;
             PRAGMA cache_size = -524288;   -- 512 MB
             PRAGMA mmap_size = 1073741824; -- 1 GB",
        )?;
        Ok(BulkWriteGuard { store: self })
    }
}

/// RAII guard that attempts best-effort schema repair on drop.
///
/// If the process crashes during a full rebuild between dropping and
/// recreating indexes, the next `init_schema()` run will detect missing
/// objects via `ensure_required_schema_objects()`. This guard provides
/// an additional safety net by attempting cleanup during normal shutdown.
pub struct FullRebuildGuard {
    store: Arc<Store>,
    active: bool,
}

impl FullRebuildGuard {
    pub fn new(store: &Arc<Store>) -> Self {
        Self {
            store: Arc::clone(store),
            active: true,
        }
    }

    /// Commit the guard — schema is complete, don't repair on drop.
    pub fn commit(mut self) {
        self.active = false;
    }
}

impl Drop for FullRebuildGuard {
    fn drop(&mut self) {
        if self.active {
            // Best-effort: on panic/unexpected exit, try to restore schema.
            // Errors are swallowed — this is crash recovery, not normal path.
            let conn_guard = self.store.lock();
            let store_ref: &Store = self.store.as_ref();
            let _ = crate::bulk_schema::execute_batch_ddl(
                &conn_guard,
                &store_ref.build_final_ddl_sqls(),
            );
            // Restore FK enforcement
            let _ = conn_guard.execute_batch("PRAGMA foreign_keys = ON;");
        }
    }
}

/// RAII guard that restores safety defaults on drop.
///
/// Acquired via [`Store::enter_bulk_write`].  On `Drop` (including
/// unwinding panics) the guard restores `synchronous = NORMAL`,
/// `foreign_keys = ON`, and normal cache/mmap sizes.
pub struct BulkWriteGuard<'s> {
    store: &'s Store,
}

impl Drop for BulkWriteGuard<'_> {
    fn drop(&mut self) {
        if let Ok(conn) = self.store.reader.conn.lock() {
            if let Err(e) = conn.execute_batch(
                "PRAGMA synchronous = NORMAL;
                 PRAGMA foreign_keys = ON;
                 PRAGMA cache_size = -65536;     -- 64 MB
                 PRAGMA mmap_size = 268435456;   -- 256 MB",
            ) {
                tracing::error!(
                    ?e,
                    "BulkWriteGuard: failed to restore PRAGMA defaults after bulk write"
                );
            }
        } else {
            tracing::error!(
                "BulkWriteGuard: mutex poisoned, cannot restore PRAGMA defaults after bulk write"
            );
        }
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
    ///
    /// **The closure MUST NOT call any other `Store` method that acquires
    /// the write lock**, such as `upsert_domain_rule`, `insert_file_facts`,
    /// or any `pub fn` that internally calls `self.lock()`.  Doing so will
    /// deadlock on the non-reentrant `std::sync::Mutex`.
    ///
    /// Prefer passing the `&Transaction` to domain helper functions
    /// (e.g. `write_symbols(tx, …)`) instead of calling `Store` methods.
    pub fn with_transaction<F, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&Transaction) -> anyhow::Result<T>,
    {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    // ============================================================
    // Bulk-load schema management (full index rebuild optimization)
    // ============================================================

    /// Drop all non-PK indexes and FTS triggers before bulk write (Phase 6).
    /// Used only during full index rebuild, NOT incremental sync.
    ///
    /// `must_rebuild` is set to `true` so that the caller knows to
    /// recreate indexes after writing.
    pub fn drop_writable_indexes(&self, must_rebuild: &mut bool) -> anyhow::Result<()> {
        let conn = self.lock();
        let mut sqls: Vec<String> = Vec::new();

        for idx in crate::bulk_schema::ALL_WRITE_INDEXES {
            sqls.push(format!("DROP INDEX IF EXISTS {idx}"));
        }
        for trigger in crate::bulk_schema::FTS_TRIGGERS {
            sqls.push(format!("DROP TRIGGER IF EXISTS {trigger}"));
        }

        tracing::info!(
            target: "atlas_db",
            drop_count = sqls.len(),
            "dropping indexes and FTS triggers for bulk write"
        );
        crate::bulk_schema::execute_batch_ddl(&conn, &sqls)?;
        *must_rebuild = true;
        Ok(())
    }

    /// Create minimal indexes needed before Phase 7 resolution.
    /// Called after Phase 6 write, before `phase_resolve_and_build`.
    pub fn create_resolution_indexes(&self) -> anyhow::Result<()> {
        let conn = self.lock();
        let mut sqls: Vec<String> = Vec::new();
        for idx in crate::bulk_schema::RESOLUTION_INDEXES {
            sqls.push(self.index_create_sql(idx));
        }
        tracing::info!(
            target: "atlas_db",
            index_count = sqls.len(),
            "creating resolution indexes"
        );
        crate::bulk_schema::execute_batch_ddl(&conn, &sqls)
    }

    /// Create summary-only indexes (dataflow/CFG) before SummaryBuild.
    /// Called only for `--analysis full`, after Phase 7, before Phase 9.
    pub fn create_summary_indexes_if_needed(&self) -> anyhow::Result<()> {
        let conn = self.lock();
        let mut sqls: Vec<String> = Vec::new();
        for idx in crate::bulk_schema::SUMMARY_INDEXES {
            sqls.push(self.index_create_sql(idx));
        }
        tracing::info!(
            target: "atlas_db",
            index_count = sqls.len(),
            "creating summary indexes"
        );
        crate::bulk_schema::execute_batch_ddl(&conn, &sqls)
    }

    /// Create all remaining indexes + rebuild FTS at Phase 10 finalize.
    /// Also commits the FullRebuildGuard if one is active.
    pub fn create_final_indexes_and_rebuild_fts(&self) -> anyhow::Result<()> {
        let conn = self.lock();

        // 1. Create all remaining query indexes
        let mut sqls: Vec<String> = Vec::new();
        for idx in crate::bulk_schema::FINAL_QUERY_INDEXES {
            sqls.push(self.index_create_sql(idx));
        }
        crate::bulk_schema::execute_batch_ddl(&conn, &sqls)?;

        // 2. Restore FTS triggers
        let fts_sqls = vec![
            crate::bulk_schema::SYMBOLS_AI_TRIGGER.to_string(),
            crate::bulk_schema::SYMBOLS_AD_TRIGGER.to_string(),
            crate::bulk_schema::SYMBOLS_AU_TRIGGER.to_string(),
        ];
        crate::bulk_schema::execute_batch_ddl(&conn, &fts_sqls)?;

        // 3. Rebuild FTS index
        tracing::info!(target: "atlas_db", "rebuilding FTS index");
        conn.execute_batch(crate::bulk_schema::FTS_REBUILD)?;

        tracing::info!(
            target: "atlas_db",
            index_count = crate::bulk_schema::FINAL_QUERY_INDEXES.len(),
            "final indexes and FTS complete"
        );
        Ok(())
    }

    /// Ensure all canonical schema objects exist.
    /// Called after `init_schema()` or during read-only open.
    /// Detects missing indexes/triggers and creates them if possible.
    /// Returns the number of schema objects (indexes + triggers) that were
    /// repaired.  A fresh schema returns 0.
    pub fn ensure_required_schema_objects(&self) -> anyhow::Result<usize> {
        let conn = self.lock();
        // Gather all expected indexes from schema
        let mut expected: std::collections::HashSet<String> = std::collections::HashSet::new();
        for idx in crate::bulk_schema::ALL_WRITE_INDEXES {
            expected.insert(idx.to_string());
        }
        // Extraction + summary indexes — kept during bulk write, but still checkable
        for idx in crate::bulk_schema::EXTRACTION_AND_SUMMARY_INDEXES {
            expected.insert(idx.to_string());
        }
        // Query existing indexes
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%'")?;
        let existing: std::collections::HashSet<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();

        let mut repaired: usize = 0;
        let missing: Vec<&String> = expected.difference(&existing).collect();
        if !missing.is_empty() {
            tracing::warn!(
                target: "atlas_db",
                missing_count = missing.len(),
                missing = ?missing.iter().take(10).collect::<Vec<_>>(),
                "detected missing schema indexes"
            );
            let mut sqls: Vec<String> = Vec::new();
            for name in &missing {
                sqls.push(self.index_create_sql(name));
            }
            crate::bulk_schema::execute_batch_ddl(&conn, &sqls)?;
            repaired += missing.len();
        }

        // Check FTS triggers
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type='trigger' AND name IN ('symbols_ai','symbols_ad','symbols_au')"
        )?;
        let existing_triggers: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        if existing_triggers.len() < 3 {
            let fts_sqls = vec![
                crate::bulk_schema::SYMBOLS_AI_TRIGGER.to_string(),
                crate::bulk_schema::SYMBOLS_AD_TRIGGER.to_string(),
                crate::bulk_schema::SYMBOLS_AU_TRIGGER.to_string(),
            ];
            crate::bulk_schema::execute_batch_ddl(&conn, &fts_sqls)?;
            repaired += 3 - existing_triggers.len();
        }

        Ok(repaired)
    }

    /// Map an index name to its CREATE INDEX IF NOT EXISTS SQL.
    /// This MUST stay in sync with the DDL in schema.rs.
    fn index_create_sql(&self, name: &str) -> String {
        match name {
            // extraction tables (kept during bulk write, but available for repair)
            "idx_extraction_state_file_layer" =>
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_extraction_state_file_layer ON extraction_state(file_id, layer) WHERE unit_id IS NULL".into(),
            "idx_extraction_state_unit_layer" =>
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_extraction_state_unit_layer ON extraction_state(file_id, unit_id, layer) WHERE unit_id IS NOT NULL".into(),
            "idx_extraction_state_file" =>
                "CREATE INDEX IF NOT EXISTS idx_extraction_state_file ON extraction_state(file_id)".into(),
            "idx_extraction_state_layer_status" =>
                "CREATE INDEX IF NOT EXISTS idx_extraction_state_layer_status ON extraction_state(layer, status)".into(),
            "idx_extraction_jobs_file_layer_status" =>
                "CREATE INDEX IF NOT EXISTS idx_extraction_jobs_file_layer_status ON extraction_jobs(file_id, layer, status)".into(),
            "idx_extraction_jobs_status" =>
                "CREATE INDEX IF NOT EXISTS idx_extraction_jobs_status ON extraction_jobs(status)".into(),
            "idx_extraction_jobs_active_file_layer" =>
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_extraction_jobs_active_file_layer ON extraction_jobs(file_id, layer) WHERE unit_id IS NULL AND status IN ('queued', 'building')".into(),
            "idx_extraction_jobs_active_unit_layer" =>
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_extraction_jobs_active_unit_layer ON extraction_jobs(file_id, unit_id, layer) WHERE unit_id IS NOT NULL AND status IN ('queued', 'building')".into(),
            // summary (not bulk-written, kept for repair)
            "idx_spr_function" =>
                "CREATE INDEX IF NOT EXISTS idx_spr_function ON summary_param_reaches(function_id)".into(),
            "idx_spr_param" =>
                "CREATE INDEX IF NOT EXISTS idx_spr_param ON summary_param_reaches(param_id)".into(),
            "idx_srs_function" =>
                "CREATE INDEX IF NOT EXISTS idx_srs_function ON summary_return_sources(function_id)".into(),
            "idx_srs_return" =>
                "CREATE INDEX IF NOT EXISTS idx_srs_return ON summary_return_sources(return_id)".into(),
            "idx_scas_function" =>
                "CREATE INDEX IF NOT EXISTS idx_scas_function ON summary_call_arg_sources(function_id)".into(),
            "idx_scas_callsite" =>
                "CREATE INDEX IF NOT EXISTS idx_scas_callsite ON summary_call_arg_sources(callsite_id)".into(),
            // fpa
            "idx_fpa_source_field" =>
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_fpa_source_field ON function_pointer_annotations(source_symbol, field_name)".into(),
            "idx_fpa_source" =>
                "CREATE INDEX IF NOT EXISTS idx_fpa_source ON function_pointer_annotations(source_symbol)".into(),
            "idx_fpa_target" =>
                "CREATE INDEX IF NOT EXISTS idx_fpa_target ON function_pointer_annotations(target_symbol)".into(),
            // files
            "idx_files_path" =>
                "CREATE INDEX IF NOT EXISTS idx_files_path ON files(path)".into(),
            "idx_files_language" =>
                "CREATE INDEX IF NOT EXISTS idx_files_language ON files(language)".into(),
            // symbols
            "idx_symbols_file" =>
                "CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_id)".into(),
            "idx_symbols_qname" =>
                "CREATE INDEX IF NOT EXISTS idx_symbols_qname ON symbols(qualified_name)".into(),
            "idx_symbols_kind" =>
                "CREATE INDEX IF NOT EXISTS idx_symbols_kind ON symbols(kind)".into(),
            "idx_symbols_container" =>
                "CREATE INDEX IF NOT EXISTS idx_symbols_container ON symbols(container_id)".into(),
            "idx_symbols_name" =>
                "CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name)".into(),
            // scopes
            "idx_scopes_file" =>
                "CREATE INDEX IF NOT EXISTS idx_scopes_file ON scopes(file_id)".into(),
            "idx_scopes_parent" =>
                "CREATE INDEX IF NOT EXISTS idx_scopes_parent ON scopes(parent_id)".into(),
            // references
            "idx_references_file" =>
                "CREATE INDEX IF NOT EXISTS idx_references_file ON \"references\"(file_id)".into(),
            "idx_references_name" =>
                "CREATE INDEX IF NOT EXISTS idx_references_name ON \"references\"(name)".into(),
            "idx_references_source" =>
                "CREATE INDEX IF NOT EXISTS idx_references_source ON \"references\"(source_symbol)".into(),
            "idx_references_resolved" =>
                "CREATE INDEX IF NOT EXISTS idx_references_resolved ON \"references\"(resolved_symbol_id)".into(),
            "idx_references_unresolved" =>
                "CREATE INDEX IF NOT EXISTS idx_references_unresolved ON \"references\"(resolved_symbol_id) WHERE resolved_symbol_id IS NULL".into(),
            // imports
            "idx_imports_file" =>
                "CREATE INDEX IF NOT EXISTS idx_imports_file ON imports(file_id)".into(),
            "idx_imports_module" =>
                "CREATE INDEX IF NOT EXISTS idx_imports_module ON imports(module)".into(),
            // symbol_edges
            "idx_symbol_edges_source" =>
                "CREATE INDEX IF NOT EXISTS idx_symbol_edges_source ON symbol_edges(source)".into(),
            "idx_symbol_edges_target" =>
                "CREATE INDEX IF NOT EXISTS idx_symbol_edges_target ON symbol_edges(target)".into(),
            "idx_symbol_edges_kind" =>
                "CREATE INDEX IF NOT EXISTS idx_symbol_edges_kind ON symbol_edges(kind)".into(),
            "idx_symbol_edges_source_kind" =>
                "CREATE INDEX IF NOT EXISTS idx_symbol_edges_source_kind ON symbol_edges(source, kind)".into(),
            // callsites
            "idx_callsites_caller" =>
                "CREATE INDEX IF NOT EXISTS idx_callsites_caller ON callsites(caller)".into(),
            "idx_callsites_reference" =>
                "CREATE INDEX IF NOT EXISTS idx_callsites_reference ON callsites(reference_id)".into(),
            // bindings
            "idx_bindings_file" =>
                "CREATE INDEX IF NOT EXISTS idx_bindings_file ON bindings(file_id)".into(),
            "idx_bindings_function" =>
                "CREATE INDEX IF NOT EXISTS idx_bindings_function ON bindings(function_id)".into(),
            "idx_bindings_symbol" =>
                "CREATE INDEX IF NOT EXISTS idx_bindings_symbol ON bindings(symbol_id)".into(),
            // binding_uses
            "idx_binding_uses_file" =>
                "CREATE INDEX IF NOT EXISTS idx_binding_uses_file ON binding_uses(file_id)".into(),
            "idx_binding_uses_binding" =>
                "CREATE INDEX IF NOT EXISTS idx_binding_uses_binding ON binding_uses(binding_id)".into(),
            "idx_binding_uses_reference" =>
                "CREATE INDEX IF NOT EXISTS idx_binding_uses_reference ON binding_uses(reference_id)".into(),
            // data_nodes
            "idx_data_nodes_file" =>
                "CREATE INDEX IF NOT EXISTS idx_data_nodes_file ON data_nodes(file_id)".into(),
            "idx_data_nodes_function" =>
                "CREATE INDEX IF NOT EXISTS idx_data_nodes_function ON data_nodes(function_id)".into(),
            "idx_data_nodes_binding" =>
                "CREATE INDEX IF NOT EXISTS idx_data_nodes_binding ON data_nodes(binding_id)".into(),
            // dataflow_edges
            "idx_dataflow_edges_source" =>
                "CREATE INDEX IF NOT EXISTS idx_dataflow_edges_source ON dataflow_edges(source)".into(),
            "idx_dataflow_edges_target" =>
                "CREATE INDEX IF NOT EXISTS idx_dataflow_edges_target ON dataflow_edges(target)".into(),
            "idx_dataflow_edges_kind" =>
                "CREATE INDEX IF NOT EXISTS idx_dataflow_edges_kind ON dataflow_edges(kind)".into(),
            // cfg_nodes
            "idx_cfg_nodes_function" =>
                "CREATE INDEX IF NOT EXISTS idx_cfg_nodes_function ON cfg_nodes(function_id)".into(),
            "idx_cfg_nodes_kind" =>
                "CREATE INDEX IF NOT EXISTS idx_cfg_nodes_kind ON cfg_nodes(kind)".into(),
            // cfg_edges
            "idx_cfg_edges_source" =>
                "CREATE INDEX IF NOT EXISTS idx_cfg_edges_source ON cfg_edges(source_node)".into(),
            "idx_cfg_edges_target" =>
                "CREATE INDEX IF NOT EXISTS idx_cfg_edges_target ON cfg_edges(target_node)".into(),
            "idx_cfg_edges_kind" =>
                "CREATE INDEX IF NOT EXISTS idx_cfg_edges_kind ON cfg_edges(kind)".into(),
            _ => {
                tracing::warn!(target: "atlas_db", index_name = name, "unknown index in repair — using generic CREATE INDEX");
                format!("CREATE INDEX IF NOT EXISTS {name} ON unknown_table(unknown_column)")
            }
        }
    }

    /// Build DDL SQL strings for all final indexes + FTS (used by FullRebuildGuard).
    fn build_final_ddl_sqls(&self) -> Vec<String> {
        let mut sqls: Vec<String> = Vec::new();
        for idx in crate::bulk_schema::ALL_WRITE_INDEXES {
            sqls.push(self.index_create_sql(idx));
        }
        sqls.push(crate::bulk_schema::SYMBOLS_AI_TRIGGER.to_string());
        sqls.push(crate::bulk_schema::SYMBOLS_AD_TRIGGER.to_string());
        sqls.push(crate::bulk_schema::SYMBOLS_AU_TRIGGER.to_string());
        sqls.push(crate::bulk_schema::FTS_REBUILD.to_string());
        sqls
    }

    // -----------------------------------------------------------------------
    // FileFacts — convenience batch insert
    // -----------------------------------------------------------------------

    /// Insert all components of a `FileFacts` in a single transaction.
    /// This is the primary write path from extraction.
    pub fn insert_file_facts(&self, facts: &FileFacts) -> anyhow::Result<()> {
        self.insert_file_facts_impl(std::slice::from_ref(facts))?;
        Ok(())
    }

    /// Batch-insert multiple `FileFacts` in a single transaction (P3: bulk write).
    ///
    /// This avoids per-file transaction overhead. All files are committed
    /// atomically. Use this for fresh/rebuild indexes; incremental sync may
    /// prefer the single-file path for finer-grained failure isolation.
    ///
    /// Returns per-table write timing for observability.
    pub fn insert_file_facts_batch(&self, batch: &[FileFacts]) -> anyhow::Result<DbWriteTiming> {
        if batch.is_empty() {
            return Ok(DbWriteTiming::default());
        }
        self.insert_file_facts_impl(batch)
    }

    /// Shared implementation: one transaction, one lock, N files.
    fn insert_file_facts_impl(&self, batch: &[FileFacts]) -> anyhow::Result<DbWriteTiming> {
        let _span = tracing::info_span!(target: "atlas_db", "db.insert_file_facts_impl", file_count = batch.len()).entered();
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let mut total_timing = DbWriteTiming::default();
        for facts in batch {
            let timing = if batch.len() > 1 {
                write_file_facts_insert_only_hot_tables(&tx, facts)?
            } else {
                write_file_facts(&tx, facts)?
            };
            total_timing.accumulate(&timing);
        }

        let t0 = Instant::now();
        tx.commit()?;
        total_timing.commit_ns = t0.elapsed().as_nanos() as u64;
        Ok(total_timing)
    }

    /// Atomically delete existing file data and insert new facts in one transaction.
    ///
    /// Used by lazy structural re-indexing to close the gap between
    /// `delete_file_data` and `insert_file_facts` (ensures no concurrent
    /// reader sees the file in a partially-deleted state).
    ///
    /// Callers must still call `invalidate_references_to_symbols_in_file` and
    /// `delete_edges_for_file_references` before this method to preserve
    /// cross-file references.
    pub fn replace_file_facts(&self, file_id: &FileId, facts: &FileFacts) -> anyhow::Result<()> {
        self.with_transaction(|tx| {
            tx.execute("DELETE FROM files WHERE file_id = ?1", params![file_id])?;
            write_file_facts(tx, facts)?;
            Ok(())
        })
    }

    /// Invalidate cross-file references, delete outgoing edges, and atomically
    /// replace a file's facts — all within a single transaction.
    ///
    /// Unlike `replace_file_facts`, callers do NOT need to separately call
    /// `invalidate_references_to_symbols_in_file` and
    /// `delete_edges_for_file_references` before calling this method.
    pub fn replace_file_facts_with_invalidation(
        &self,
        file_id: &FileId,
        facts: &FileFacts,
    ) -> anyhow::Result<()> {
        self.with_transaction(|tx| {
            // Any unchanged file that resolved a reference into the replaced
            // file must leave the canonical-resolution fast path. Its source
            // hash is unchanged, but its resolution context is not.
            tx.execute(
                r#"UPDATE extraction_state SET resolution_fingerprint = NULL
                   WHERE layer = 'resolution' AND unit_id IS NULL
                     AND file_id IN (
                       SELECT DISTINCT r.file_id FROM "references" r
                       WHERE r.resolved_symbol_id IN (
                           SELECT symbol_id FROM symbols WHERE file_id = ?1
                       )
                     )"#,
                params![file_id],
            )?;
            // Invalidate cross-file references pointing to this file's symbols.
            tx.execute(
                r#"UPDATE "references" SET
                    resolved_symbol_id = NULL,
                    resolved_confidence = NULL,
                    resolved_strategy = NULL,
                    resolved_provenance = NULL
                   WHERE resolved_symbol_id IN (
                        SELECT symbol_id FROM symbols WHERE file_id = ?1
                   )"#,
                params![file_id],
            )?;
            // Delete outgoing edges derived from this file's references.
            tx.execute(
                r#"DELETE FROM symbol_edges WHERE ref_id IN (
                    SELECT reference_id FROM "references" WHERE file_id = ?1
                )"#,
                params![file_id],
            )?;
            // Delete incoming edges that target symbols belonging to this
            // file. The target column has no FK, so CASCADE from files→symbols
            // does not reach these rows.
            tx.execute(
                r#"DELETE FROM symbol_edges WHERE target IN (
                    SELECT symbol_id FROM symbols WHERE file_id = ?1
                )"#,
                params![file_id],
            )?;
            // Atomically delete old facts and insert new ones.
            tx.execute("DELETE FROM files WHERE file_id = ?1", params![file_id])?;
            write_file_facts(tx, facts)?;
            Ok(())
        })
    }

    /// Upsert symbols, scopes, and imports from a ResolutionSymbols extraction.
    ///
    /// # Content hash contract
    ///
    /// When the parsed content hash differs from the stored file hash, this
    /// method atomically updates `files.content_hash` in the same transaction.
    /// This means:
    /// - The new resolution_symbols layer is consistent with the on-disk content.
    /// - Pre-existing layers (manifest, structural) with the old hash become
    ///   stale: their file-level extraction state hash no longer matches
    ///   `files.content_hash`, so `has_complete_layer()` returns `false` for
    ///   them.
    /// - On the next lazy access, those stale layers are rebuilt from current
    ///   content.
    ///
    /// This is the "safe update file row" strategy (as opposed to "reject
    /// stale"): progressive enrichment of a file never silently serves stale
    /// data.
    pub fn upsert_resolution_symbols(
        &self,
        file_id: &FileId,
        facts: &FileFacts,
    ) -> anyhow::Result<()> {
        self.with_transaction(|tx| {
            // Sync files.content_hash if it has changed since the manifest
            // index.  The layer hash must match the DB file hash for
            // `has_complete_layer()` to recognise the layer as complete.
            let db_hash: Option<String> = match tx.query_row(
                "SELECT content_hash FROM files WHERE file_id = ?1",
                params![file_id],
                |row| row.get(0),
            ) {
                Ok(hash) => Some(hash),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(e.into()),
            };
            if let Some(ref db_hash) = db_hash
                && db_hash != &facts.file.content_hash
            {
                tx.execute(
                    "UPDATE files SET content_hash = ?1 WHERE file_id = ?2",
                    params![facts.file.content_hash, file_id],
                )?;
            }

            // Upsert symbols with resolution_symbols layer tag
            if !facts.symbols.is_empty() {
                write_symbols(tx, &facts.symbols, "resolution_symbols")?;
            }
            // Upsert scopes
            if !facts.scopes.is_empty() {
                write_scopes(tx, &facts.scopes)?;
            }
            // Upsert imports
            if !facts.imports.is_empty() {
                write_imports(tx, &facts.imports)?;
            }
            // Record the resolution_symbols layer
            tx.execute(
                "DELETE FROM extraction_state
                 WHERE file_id = ?1 AND unit_id IS NULL AND layer = 'resolution_symbols'",
                params![file_id],
            )?;
            tx.execute(
                "INSERT INTO extraction_state
                    (file_id, unit_id, layer, content_hash, status, capability_mask, updated_at)
                 VALUES (?1, NULL, 'resolution_symbols', ?2, 'complete', ?3, datetime('now'))",
                params![
                    file_id,
                    facts.file.content_hash,
                    FactCoverage::MANIFEST as i64
                ],
            )?;
            Ok(())
        })
    }

    /// Query distinct function names from the symbol table for rule learning.
    pub fn query_function_names(&self) -> anyhow::Result<Vec<String>> {
        let conn = self.lock_read();
        let mut stmt =
            conn.prepare("SELECT DISTINCT name FROM symbols WHERE kind = 'function' LIMIT 5000")?;
        let names: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(names)
    }

    /// Read the resolution fingerprint for a file (P3: per-file resolution skip).
    ///
    /// Returns `None` if no fingerprint record exists (file has never been resolved),
    /// or if the fingerprint was explicitly cleared (NULL).
    /// The fingerprint is stored in `extraction_state` with layer = 'resolution'.
    pub fn get_resolution_fingerprint(&self, file_id: &FileId) -> anyhow::Result<Option<String>> {
        let conn = self.lock_read();
        let result: Result<Option<String>, _> = conn.query_row(
            "SELECT resolution_fingerprint FROM extraction_state
             WHERE file_id = ?1 AND unit_id IS NULL AND layer = 'resolution'",
            params![file_id],
            |row| row.get(0),
        );
        match result {
            Ok(Some(fp)) => Ok(Some(fp)),
            Ok(None) => Ok(None),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Update the resolution fingerprint for a file (P3: per-file resolution skip).
    ///
    /// Stores the file's `content_hash` as the fingerprint. After resolution
    /// completes, this fingerprint acts as a cache key: if the file's content
    /// hasn't changed, resolution can be skipped on the next run.
    pub fn update_resolution_fingerprint(
        &self,
        file_id: &FileId,
        fingerprint: &str,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM extraction_state
             WHERE file_id = ?1 AND unit_id IS NULL AND layer = 'resolution'",
            params![file_id],
        )?;
        conn.execute(
            "INSERT INTO extraction_state
                (file_id, unit_id, layer, content_hash, status, resolution_fingerprint, capability_mask, updated_at)
             VALUES (?1, NULL, 'resolution', '', 'complete', ?2, 0, datetime('now'))",
            params![file_id, fingerprint],
        )?;
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

/// SQLite page-cache and file-size diagnostics.
///
/// These are sampled at query time via cheap PRAGMAs (< 1 ms).
/// Together they describe how much of the database resides in the
/// operating-system page cache vs on-disk pages.
#[derive(Debug, Clone)]
pub struct SqliteCacheStats {
    /// Total pages in the database file (`PRAGMA page_count`).
    pub page_count: i64,
    /// Bytes per page (`PRAGMA page_size`).
    pub page_size: i64,
    /// Unused (free) pages (`PRAGMA freelist_count`).  A non-zero value
    /// after bulk writes means a `VACUUM` could compact the file, but
    /// free pages are cheap for SQLite (WAL auto-reclaims them).
    pub freelist_count: i64,
    /// Configured page-cache size in KiB (`PRAGMA cache_size`).
    pub cache_size_kib: i64,
    /// Size of the database file on disk, in bytes (`std::fs::metadata`).
    pub db_file_size_bytes: u64,
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
    fn find_latest_visible_reference_target(
        &self,
        reference_id: &ReferenceId,
    ) -> anyhow::Result<Option<SymbolId>> {
        Store::find_latest_visible_reference_target(self, reference_id)
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
    fn has_dataflow_for_file(&self, file_id: &FileId) -> anyhow::Result<bool> {
        Store::has_dataflow_for_file(self, file_id)
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
    fn find_dataflow_edges_by_function(
        &self,
        function_id: &SymbolId,
    ) -> anyhow::Result<Vec<DataFlowEdge>> {
        Store::find_dataflow_edges_by_function(self, function_id)
    }
    fn find_dataflow_edges_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<DataFlowEdge>> {
        Store::find_dataflow_edges_by_file(self, file_id)
    }
}

impl CallGraphReader for Store {
    fn find_callsites_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<Callsite>> {
        Store::find_callsites_by_file(self, file_id)
    }
    fn find_callsites_by_name_and_receiver(
        &self,
        name: &str,
        receiver: &str,
        language: types::enums::Language,
    ) -> anyhow::Result<Vec<Callsite>> {
        Store::find_callsites_by_name_and_receiver(self, name, receiver, language)
    }
    fn find_resolved_callsites_by_callee(
        &self,
        callee: &SymbolId,
    ) -> anyhow::Result<Vec<ResolvedCallsite>> {
        Store::find_resolved_callsites_by_callee(self, callee)
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
    fn find_resolved_callsites_by_id(
        &self,
        id: &CallsiteId,
    ) -> anyhow::Result<Vec<ResolvedCallsite>> {
        Store::find_resolved_callsites_by_id(self, id)
    }
    fn find_resolved_callsite_by_reference_id(
        &self,
        reference_id: &ReferenceId,
    ) -> anyhow::Result<Option<ResolvedCallsite>> {
        Store::find_resolved_callsite_by_reference_id(self, reference_id)
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
    fn find_files_by_path_prefix(&self, prefix: &str) -> anyhow::Result<Vec<FileInfo>> {
        Store::find_files_by_path_prefix(self, prefix)
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
mod closure_coverage_tests;
#[cfg(test)]
mod closure_generations_tests;
#[cfg(test)]
mod reference_resolutions_tests;

#[cfg(test)]
mod file_inventory_tests;
#[cfg(test)]
mod symbol_hints_tests;

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
            qualified_name: format!("{name}.{name}"),
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
        store.insert_symbols(std::slice::from_ref(&sym)).unwrap();

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
        store.insert_symbols(std::slice::from_ref(&sym)).unwrap();

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
            id: ref_id,
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
    fn find_references_by_symbol_reads_visible_focus_resolution() {
        let store = test_store();
        let file = test_file();
        store.upsert_file(&file).unwrap();

        let source = test_symbol(file.file_id, "caller", SymbolKind::Function);
        let target = test_symbol(file.file_id, "target", SymbolKind::Function);
        store
            .insert_symbols(&[source.clone(), target.clone()])
            .unwrap();

        let range = TextRange {
            start_byte: 50,
            end_byte: 56,
            start_line: 3,
            start_column: 5,
            end_line: 3,
            end_column: 11,
        };
        let reference = ReferenceUse {
            id: ReferenceId::generate(
                &file.file_id,
                Some(&source.id),
                range.start_byte,
                range.end_byte,
                "target",
                ReferenceKind::Call,
            ),
            file_id: file.file_id,
            source_symbol: Some(source.id),
            scope_id: None,
            kind: ReferenceKind::Call,
            text: "target".into(),
            name: "target".into(),
            receiver: None,
            arity: Some(0),
            range,
            binding_id: None,
            resolved: None,
        };
        store
            .insert_references(std::slice::from_ref(&reference))
            .unwrap();
        store.insert_closure_generation("cl_usages").unwrap();
        store
            .insert_reference_resolution(
                reference.id.as_bytes(),
                "cl_usages",
                1,
                "closure_reachable",
                Some(target.id.as_bytes()),
                "closure_complete",
                "high",
                "closure_reachable",
                None,
            )
            .unwrap();

        assert!(
            store
                .find_references_by_symbol(&target.id)
                .unwrap()
                .is_empty()
        );
        store.make_resolutions_visible("cl_usages", 1).unwrap();

        let usages = store.find_references_by_symbol(&target.id).unwrap();
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].id, reference.id);
    }

    #[test]
    fn test_find_references_by_name_and_kind_in_scope() {
        let store = test_store();
        let file = test_file();
        store.upsert_file(&file).unwrap();

        let sym = test_symbol(file.file_id, "Foo", SymbolKind::Struct);
        store.insert_symbols(std::slice::from_ref(&sym)).unwrap();

        // Two decoration references for @Component, one call reference named
        // "Component" (should NOT match the decoration query).
        let dec_range = TextRange {
            start_byte: 0,
            end_byte: 11,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 11,
        };
        let dec_ref = ReferenceUse {
            id: ReferenceId::generate(
                &file.file_id,
                Some(&sym.id),
                dec_range.start_byte,
                dec_range.end_byte,
                "Component",
                ReferenceKind::Decoration,
            ),
            file_id: file.file_id,
            source_symbol: Some(sym.id),
            scope_id: None,
            kind: ReferenceKind::Decoration,
            text: "Component".into(),
            name: "Component".into(),
            receiver: None,
            arity: None,
            range: dec_range,
            binding_id: None,
            resolved: None,
        };
        let call_ref = ReferenceUse {
            id: ReferenceId::generate(
                &file.file_id,
                Some(&sym.id),
                100,
                110,
                "Component",
                ReferenceKind::Call,
            ),
            file_id: file.file_id,
            source_symbol: Some(sym.id),
            scope_id: None,
            kind: ReferenceKind::Call,
            text: "Component".into(),
            name: "Component".into(),
            receiver: None,
            arity: None,
            range: TextRange {
                start_byte: 100,
                end_byte: 110,
                start_line: 5,
                start_column: 0,
                end_line: 5,
                end_column: 10,
            },
            binding_id: None,
            resolved: None,
        };
        store
            .insert_references(&[dec_ref.clone(), call_ref])
            .unwrap();

        // Decoration lookup by name should return exactly 1 reference.
        let decorations = store
            .find_references_by_name_and_kind_in_scope(
                "Component",
                ReferenceKind::Decoration,
                "src",
            )
            .unwrap();
        assert_eq!(decorations.len(), 1);
        assert_eq!(decorations[0].id, dec_ref.id);
        assert_eq!(decorations[0].kind, ReferenceKind::Decoration);

        // A different name should return nothing.
        let empty = store
            .find_references_by_name_and_kind_in_scope("State", ReferenceKind::Decoration, "src")
            .unwrap();
        assert!(empty.is_empty());

        let outside_scope = store
            .find_references_by_name_and_kind_in_scope(
                "Component",
                ReferenceKind::Decoration,
                "other",
            )
            .unwrap();
        assert!(outside_scope.is_empty());
    }

    #[test]
    fn test_find_unresolved_call_references_by_source() {
        let store = test_store();
        let file = test_file();
        store.upsert_file(&file).unwrap();

        let src = test_symbol(file.file_id, "caller", SymbolKind::Function);
        let other = test_symbol(file.file_id, "other", SymbolKind::Function);
        store.insert_symbols(&[src.clone(), other.clone()]).unwrap();

        let range = TextRange {
            start_byte: 50,
            end_byte: 64,
            start_line: 3,
            start_column: 5,
            end_line: 3,
            end_column: 19,
        };
        let call_ref = ReferenceUse {
            id: ReferenceId::generate(
                &file.file_id,
                Some(&src.id),
                range.start_byte,
                range.end_byte,
                "copy_from_user",
                ReferenceKind::Call,
            ),
            file_id: file.file_id,
            source_symbol: Some(src.id),
            scope_id: None,
            kind: ReferenceKind::Call,
            text: "copy_from_user".into(),
            name: "copy_from_user".into(),
            receiver: None,
            arity: Some(3),
            range,
            binding_id: None,
            resolved: None,
        };
        let usage_ref = ReferenceUse {
            id: ReferenceId::generate(
                &file.file_id,
                Some(&src.id),
                70,
                74,
                "flag",
                ReferenceKind::Usage,
            ),
            file_id: file.file_id,
            source_symbol: Some(src.id),
            scope_id: None,
            kind: ReferenceKind::Usage,
            text: "flag".into(),
            name: "flag".into(),
            receiver: None,
            arity: None,
            range: TextRange {
                start_byte: 70,
                end_byte: 74,
                start_line: 4,
                start_column: 1,
                end_line: 4,
                end_column: 5,
            },
            binding_id: None,
            resolved: None,
        };
        let other_call = ReferenceUse {
            id: ReferenceId::generate(
                &file.file_id,
                Some(&other.id),
                80,
                86,
                "helper",
                ReferenceKind::Call,
            ),
            file_id: file.file_id,
            source_symbol: Some(other.id),
            scope_id: None,
            kind: ReferenceKind::Call,
            text: "helper".into(),
            name: "helper".into(),
            receiver: None,
            arity: None,
            range: TextRange {
                start_byte: 80,
                end_byte: 86,
                start_line: 5,
                start_column: 1,
                end_line: 5,
                end_column: 7,
            },
            binding_id: None,
            resolved: None,
        };
        store
            .insert_references(&[call_ref.clone(), usage_ref, other_call])
            .unwrap();

        let unresolved_calls = store
            .find_unresolved_call_references_by_source(&src.id)
            .unwrap();
        assert_eq!(unresolved_calls.len(), 1);
        assert_eq!(unresolved_calls[0].id, call_ref.id);
        assert_eq!(unresolved_calls[0].name, "copy_from_user");

        store.insert_closure_generation("cl_resolved_call").unwrap();
        store
            .insert_reference_resolution(
                call_ref.id.as_bytes(),
                "cl_resolved_call",
                1,
                "closure_reachable",
                Some(other.id.as_bytes()),
                "closure_complete",
                "high",
                "exact_match",
                Some("focus_closure"),
            )
            .unwrap();
        store
            .make_resolutions_visible("cl_resolved_call", 1)
            .unwrap();

        assert!(
            store
                .find_unresolved_call_references_by_source(&src.id)
                .unwrap()
                .is_empty(),
            "visible closure resolutions must not also appear as unresolved calls"
        );
        assert_eq!(
            store
                .find_latest_visible_reference_target(&call_ref.id)
                .unwrap(),
            Some(other.id)
        );
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
            id: ref_id,
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
    fn replacing_target_facts_revokes_importer_resolution_fingerprint() {
        let store = test_store();
        let target_id = FileId::generate("src/target.ts");
        let target_symbol = test_symbol(target_id, "target", SymbolKind::Function);
        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id: target_id,
                    path: "src/target.ts".into(),
                    language: Language::TypeScript,
                    content_hash: "target-v1".into(),
                    status: ParseStatus::Success,
                },
                symbols: vec![target_symbol.clone()],
                ..Default::default()
            })
            .unwrap();

        let caller_id = FileId::generate("src/caller.ts");
        let caller_symbol = test_symbol(caller_id, "caller", SymbolKind::Function);
        let range = TextRange::default();
        let reference_id = ReferenceId::generate(
            &caller_id,
            Some(&caller_symbol.id),
            range.start_byte,
            range.end_byte,
            "target",
            ReferenceKind::Call,
        );
        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id: caller_id,
                    path: "src/caller.ts".into(),
                    language: Language::TypeScript,
                    content_hash: "caller-v1".into(),
                    status: ParseStatus::Success,
                },
                symbols: vec![caller_symbol.clone()],
                references: vec![ReferenceUse {
                    id: reference_id,
                    file_id: caller_id,
                    source_symbol: Some(caller_symbol.id),
                    scope_id: None,
                    kind: ReferenceKind::Call,
                    text: "target".into(),
                    name: "target".into(),
                    receiver: None,
                    arity: Some(0),
                    range,
                    binding_id: None,
                    resolved: None,
                }],
                ..Default::default()
            })
            .unwrap();
        store
            .update_reference_resolution(
                &reference_id,
                &ResolvedTarget {
                    symbol_id: target_symbol.id,
                    confidence: Confidence::certain(),
                    strategy: ResolutionStrategy::ExactMatch,
                    provenance: Provenance::TreeSitter,
                },
            )
            .unwrap();
        store
            .update_resolution_fingerprint(&caller_id, "caller-v1")
            .unwrap();

        store
            .replace_file_facts_with_invalidation(
                &target_id,
                &FileFacts {
                    file: FileInfo {
                        file_id: target_id,
                        path: "src/target.ts".into(),
                        language: Language::TypeScript,
                        content_hash: "target-v2".into(),
                        status: ParseStatus::Success,
                    },
                    symbols: vec![target_symbol],
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(store.get_resolution_fingerprint(&caller_id).unwrap(), None);
        assert!(!store.scope_has_current_resolution_fingerprint("").unwrap());
        assert!(
            store
                .find_unresolved_references()
                .unwrap()
                .iter()
                .any(|reference| reference.id == reference_id)
        );
    }

    #[test]
    fn insert_file_facts_keeps_replace_semantics_for_hot_tables() {
        let store = test_store();
        let file_id = FileId::generate("src/repeat.ts");
        let sym = test_symbol(file_id, "caller", SymbolKind::Function);
        let range = TextRange {
            start_byte: 10,
            end_byte: 20,
            start_line: 2,
            start_column: 1,
            end_line: 2,
            end_column: 11,
        };
        let ref_id = ReferenceId::generate(
            &file_id,
            Some(&sym.id),
            range.start_byte,
            range.end_byte,
            "target",
            ReferenceKind::Call,
        );
        let reference = ReferenceUse {
            id: ref_id,
            file_id,
            source_symbol: Some(sym.id),
            scope_id: None,
            kind: ReferenceKind::Call,
            text: "target()".to_string(),
            name: "target".to_string(),
            receiver: None,
            arity: Some(0),
            range,
            binding_id: None,
            resolved: None,
        };
        let callsite = Callsite {
            id: CallsiteId::generate(&ref_id, Some(&sym.id), range.start_byte),
            reference_id: Some(ref_id),
            caller: sym.id,
            receiver: None,
            args: vec![],
            range,
            callee_range: Some(range),
        };
        let facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "src/repeat.ts".into(),
                language: Language::TypeScript,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            },
            symbols: vec![sym],
            references: vec![reference],
            callsites: vec![callsite],
            ..Default::default()
        };

        store.insert_file_facts(&facts).unwrap();
        store.insert_file_facts(&facts).unwrap();

        let conn = store.lock_read();
        let refs: i64 = conn
            .query_row(r#"SELECT COUNT(*) FROM "references""#, [], |row| row.get(0))
            .unwrap();
        let callsites: i64 = conn
            .query_row("SELECT COUNT(*) FROM callsites", [], |row| row.get(0))
            .unwrap();
        assert_eq!(refs, 1);
        assert_eq!(callsites, 1);
    }

    #[test]
    fn unresolved_callsite_is_absent_from_resolved_lookups() {
        let store = test_store();
        let file_id = FileId::generate("src/unresolved.ts");
        let caller = test_symbol(file_id, "caller", SymbolKind::Function);
        let caller_id = caller.id;
        let range = TextRange {
            start_byte: 10,
            end_byte: 18,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 9,
        };
        let reference_id = ReferenceId::generate(
            &file_id,
            Some(&caller_id),
            range.start_byte,
            range.end_byte,
            "external",
            ReferenceKind::Call,
        );
        let callsite_id = CallsiteId::generate(&reference_id, Some(&caller_id), range.start_byte);
        let facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "src/unresolved.ts".into(),
                language: Language::TypeScript,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            },
            symbols: vec![caller],
            references: vec![ReferenceUse {
                id: reference_id,
                file_id,
                source_symbol: Some(caller_id),
                scope_id: None,
                kind: ReferenceKind::Call,
                text: "external()".into(),
                name: "external".into(),
                receiver: None,
                arity: Some(0),
                range,
                binding_id: None,
                resolved: None,
            }],
            callsites: vec![Callsite {
                id: callsite_id,
                reference_id: Some(reference_id),
                caller: caller_id,
                receiver: None,
                args: vec![],
                range,
                callee_range: Some(range),
            }],
            ..Default::default()
        };
        store.insert_file_facts(&facts).unwrap();

        assert!(
            store
                .find_resolved_callsites_by_id(&callsite_id)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .find_resolved_callsite_by_reference_id(&reference_id)
                .unwrap()
                .is_none()
        );
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
        assert!(!stats.sqlite_version.is_empty());
    }

    // ── FK guard tests for replace_dataflow_for_unit ────────────────────

    /// Regression: `replace_dataflow_for_unit` should not crash with FK
    /// constraint failure when data nodes reference a non-existent function
    /// symbol.  The invalid rows are silently dropped.
    #[test]
    fn replace_dataflow_survives_missing_function_id() {
        let store = test_store();
        let file_id = FileId::generate("src/example.c");
        let range = TextRange {
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 1,
            end_line: 10,
            end_column: 1,
        };

        // Insert the file row (required FK for data_nodes.file_id)
        let file_info = FileInfo {
            file_id,
            path: "src/example.c".into(),
            language: Language::C,
            content_hash: "abc".into(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file_info).unwrap();

        // Create a valid function symbol and scope
        let valid_func = test_symbol(file_id, "existing_func", SymbolKind::Function);
        let valid_scope_id = ScopeId::generate(&file_id, None, "function", 0);
        let valid_scope = ScopeDef {
            id: valid_scope_id,
            file_id,
            kind: ScopeKind::Function,
            name: "existing_func_scope".into(),
            scope_path: "existing_func_scope".into(),
            range,
            parent_id: None,
        };

        // Insert the symbol + scope so they exist in DB
        store
            .insert_file_facts(&FileFacts {
                file: file_info.clone(),
                symbols: vec![valid_func.clone()],
                scopes: vec![valid_scope],
                ..Default::default()
            })
            .unwrap();

        // A non-existent symbol ID (never inserted)
        let missing_func_id = SymbolId::generate(
            &file_id,
            "c",
            "nonexistent_func",
            SymbolKind::Function.as_str(),
            None,
        );

        // Build test data: one valid binding + one with missing function_id
        let valid_binding_id =
            BindingId::generate(&file_id, &valid_scope_id, "parameter", "arg", 0);
        let stray_binding_id =
            BindingId::generate(&file_id, &valid_scope_id, "parameter", "stray", 1);

        let bindings = vec![
            // Valid: references existing function and scope
            BindingDef {
                id: valid_binding_id,
                file_id,
                function_id: Some(valid_func.id),
                scope_id: valid_scope_id,
                kind: types::enums::BindingKind::Parameter,
                name: "arg".into(),
                symbol_id: None,
                visible_from_byte: range.start_byte,
                range,
            },
            // Invalid: references non-existent function
            BindingDef {
                id: stray_binding_id,
                file_id,
                function_id: Some(missing_func_id),
                scope_id: valid_scope_id,
                kind: types::enums::BindingKind::Local,
                name: "stray".into(),
                symbol_id: None,
                visible_from_byte: range.start_byte,
                range,
            },
        ];

        let data_nodes = vec![
            // Valid: references existing function + valid binding
            DataNode::parameter(
                DataNodeId::generate(
                    &file_id,
                    Some(&valid_func.id),
                    "parameter",
                    Some("arg"),
                    Some("arg"),
                    15,
                ),
                file_id,
                Some(valid_func.id),
                Some(valid_binding_id),
                "arg",
                range,
            ),
            // Invalid: references non-existent function
            DataNode::parameter(
                DataNodeId::generate(
                    &file_id,
                    Some(&missing_func_id),
                    "parameter",
                    Some("ghost"),
                    Some("ghost"),
                    20,
                ),
                file_id,
                Some(missing_func_id),
                None,
                "ghost",
                range,
            ),
        ];

        // Dataflow edge referencing both nodes — should be filtered because
        // one target (ghost) will be removed
        let edge = DataFlowEdge {
            id: DataFlowEdgeId::generate(&data_nodes[0].id, &data_nodes[1].id, "assign"),
            source: data_nodes[0].id,
            target: data_nodes[1].id,
            kind: DataFlowKind::Assign,
            location: range,
            confidence: 1.0,
        };

        let unit = types::lazy::AnalysisUnit::from_function(file_id, valid_func.id, range);

        // This must NOT panic or fail with FK constraint
        store
            .replace_dataflow_for_unit(
                &unit,
                &data_nodes,
                &[edge],
                &bindings,
                &[], // no binding_uses
                &[], // no cfg_nodes
                &[], // no cfg_edges
            )
            .unwrap();

        // After FK-guarded write, only the valid rows should exist
        let stored_nodes = store.find_data_nodes_by_file(&file_id).unwrap();
        assert_eq!(
            stored_nodes.len(),
            1,
            "only the valid data node should be stored"
        );
        assert_eq!(stored_nodes[0].name.as_deref(), Some("arg"));

        let stored_bindings = store.find_bindings_by_file(&file_id).unwrap();
        // Note: insert_file_facts may also write bindings, so we count the ones
        // with our test binding IDs.
        let our_bindings: Vec<_> = stored_bindings
            .iter()
            .filter(|b| b.id == valid_binding_id)
            .collect();
        assert_eq!(our_bindings.len(), 1, "valid binding should be stored");

        let our_stray: Vec<_> = stored_bindings
            .iter()
            .filter(|b| b.id == stray_binding_id)
            .collect();
        assert!(our_stray.is_empty(), "stray binding should be filtered out");

        // Dataflow edges referencing removed nodes should also be dropped
        let all_edges = store.find_dataflow_edges_by_file(&file_id).unwrap();
        assert!(
            all_edges.is_empty(),
            "edge with missing target should be filtered out"
        );
    }

    /// `replace_dataflow_for_unit` with fully valid FK references writes
    /// all rows correctly.
    #[test]
    fn replace_dataflow_with_valid_fks_writes_correctly() {
        let store = test_store();
        let file_id = FileId::generate("src/valid.c");
        let range = TextRange {
            start_byte: 0,
            end_byte: 50,
            start_line: 1,
            start_column: 1,
            end_line: 5,
            end_column: 1,
        };

        let file_info = FileInfo {
            file_id,
            path: "src/valid.c".into(),
            language: Language::C,
            content_hash: "abc".into(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file_info).unwrap();

        let func = test_symbol(file_id, "my_func", SymbolKind::Function);
        let scope_id = ScopeId::generate(&file_id, None, "function", 0);
        let scope = ScopeDef {
            id: scope_id,
            file_id,
            kind: ScopeKind::Function,
            name: "my_func_scope".into(),
            scope_path: "my_func_scope".into(),
            range,
            parent_id: None,
        };

        store
            .insert_file_facts(&FileFacts {
                file: file_info.clone(),
                symbols: vec![func.clone()],
                scopes: vec![scope],
                ..Default::default()
            })
            .unwrap();

        let binding_id = BindingId::generate(&file_id, &scope_id, "parameter", "x", 0);
        let bindings = vec![BindingDef {
            id: binding_id,
            file_id,
            function_id: Some(func.id),
            scope_id,
            kind: types::enums::BindingKind::Parameter,
            name: "x".into(),
            symbol_id: None,
            visible_from_byte: range.start_byte,
            range,
        }];

        let dn_id = DataNodeId::generate(
            &file_id,
            Some(&func.id),
            "parameter",
            Some("x"),
            Some("x"),
            10,
        );
        let data_nodes = vec![DataNode::parameter(
            dn_id,
            file_id,
            Some(func.id),
            Some(binding_id),
            "x",
            range,
        )];

        let unit = types::lazy::AnalysisUnit::from_function(file_id, func.id, range);
        store
            .replace_dataflow_for_unit(&unit, &data_nodes, &[], &bindings, &[], &[], &[])
            .unwrap();

        let got = store.find_data_nodes_by_file(&file_id).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name.as_deref(), Some("x"));

        let bindings = store.find_bindings_by_file(&file_id).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].name, "x");
    }

    /// `find_data_node_at_range` must push the kind + byte-range predicate
    /// into SQL so callers never load a whole file's data nodes just to pick
    /// one. It must match on all three dimensions (file, kind, exact range).
    #[test]
    fn find_data_node_at_range_matches_kind_and_exact_range() {
        let store = test_store();
        let file_id = FileId::generate("src/fp.c");
        let func_range = TextRange {
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 1,
            end_line: 10,
            end_column: 1,
        };

        let file_info = FileInfo {
            file_id,
            path: "src/fp.c".into(),
            language: Language::C,
            content_hash: "abc".into(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file_info).unwrap();

        let func = test_symbol(file_id, "caller", SymbolKind::Function);
        store
            .insert_file_facts(&FileFacts {
                file: file_info.clone(),
                symbols: vec![func.clone()],
                ..Default::default()
            })
            .unwrap();

        let node_range = |start_byte: u32, end_byte: u32| TextRange {
            start_byte,
            end_byte,
            start_line: 2,
            start_column: 1,
            end_line: 2,
            end_column: 1,
        };

        // Three nodes in the same file: two call targets at different ranges,
        // plus a call arg that overlaps one of them exactly.
        let target_a = node_range(10, 20);
        let target_b = node_range(30, 40);
        let data_nodes = vec![
            DataNode::call_target(
                DataNodeId::generate(&file_id, Some(&func.id), "call_target", Some("a"), None, 10),
                file_id,
                Some(func.id),
                None,
                "a",
                "a",
                target_a,
            ),
            DataNode::call_target(
                DataNodeId::generate(&file_id, Some(&func.id), "call_target", Some("b"), None, 30),
                file_id,
                Some(func.id),
                None,
                "b",
                "b",
                target_b,
            ),
            DataNode::call_arg(
                DataNodeId::generate(&file_id, Some(&func.id), "call_arg", Some("arg"), None, 10),
                file_id,
                Some(func.id),
                None,
                Some("arg"),
                target_a,
            ),
        ];

        let unit = types::lazy::AnalysisUnit::from_function(file_id, func.id, func_range);
        store
            .replace_dataflow_for_unit(&unit, &data_nodes, &[], &[], &[], &[], &[])
            .unwrap();

        // Exact range + kind picks the right node, not the overlapping call_arg.
        let hit = store
            .find_data_node_at_range(&file_id, DataNodeKind::CallTarget, 10, 20)
            .unwrap()
            .expect("call target at 10..20");
        assert_eq!(hit.name.as_deref(), Some("a"));
        assert_eq!(hit.kind, DataNodeKind::CallTarget);

        let hit_b = store
            .find_data_node_at_range(&file_id, DataNodeKind::CallTarget, 30, 40)
            .unwrap()
            .expect("call target at 30..40");
        assert_eq!(hit_b.name.as_deref(), Some("b"));

        // Same range, different kind.
        let arg = store
            .find_data_node_at_range(&file_id, DataNodeKind::CallArg, 10, 20)
            .unwrap()
            .expect("call arg at 10..20");
        assert_eq!(arg.name.as_deref(), Some("arg"));

        // A range that no node occupies yields None rather than a near match.
        assert!(
            store
                .find_data_node_at_range(&file_id, DataNodeKind::CallTarget, 10, 21)
                .unwrap()
                .is_none()
        );

        // Another file with the same range must not leak across.
        let other_file = FileId::generate("src/other.c");
        assert!(
            store
                .find_data_node_at_range(&other_file, DataNodeKind::CallTarget, 10, 20)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn replace_top_level_dataflow_does_not_query_function_cfg() {
        let store = test_store();
        let file_id = FileId::generate("src/top_level.ts");
        let range = TextRange {
            start_byte: 0,
            end_byte: 20,
            start_line: 0,
            start_column: 0,
            end_line: 1,
            end_column: 0,
        };
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "src/top_level.ts".into(),
                language: Language::TypeScript,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        let unit = types::lazy::AnalysisUnit::from_top_level(file_id, range);
        store
            .replace_dataflow_for_unit(&unit, &[], &[], &[], &[], &[], &[])
            .unwrap();
    }

    // ── C #include dependents tests ─────────────────────────────────────

    /// `find_dependents_by_file` resolves C `#include "helper.h"` directives
    /// via basename matching when the LIKE query returns empty.
    #[test]
    fn dependents_resolves_c_bare_include() {
        let store = test_store();

        let main_id = FileId::generate("src/main.c");
        let helper_id = FileId::generate("src/helper.h");

        // Register both files
        store
            .upsert_file(&FileInfo {
                file_id: main_id,
                path: "src/main.c".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file(&FileInfo {
                file_id: helper_id,
                path: "src/helper.h".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        // Create an import: `#include "helper.h"` in main.c
        let import = ImportDef {
            id: ImportId::generate(&main_id, "include", "helper.h", None, 0),
            file_id: main_id,
            kind: ImportKind::Include,
            module: "helper.h".into(),
            imported_name: String::new(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: true, // local include
            range: Default::default(),
        };

        // Use insert_file_facts to get FK-guarded import insertion
        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id: main_id,
                    path: "src/main.c".into(),
                    language: Language::C,
                    content_hash: "abc".into(),
                    status: ParseStatus::Success,
                },
                imports: vec![import],
                ..Default::default()
            })
            .unwrap();

        let dependents = store.find_dependents_by_file(&helper_id).unwrap();
        assert!(
            !dependents.is_empty(),
            "main.c should be found as a dependent of helper.h"
        );
        assert!(
            dependents.iter().any(|(path, _mod)| path == "src/main.c"),
            "expected src/main.c in dependents, got: {dependents:?}"
        );
    }

    /// `find_dependents_by_file` handles C relative-path includes like
    /// `#include "dir/helper.h"` where the importing file is in a different
    /// directory.
    #[test]
    fn dependents_resolves_c_relative_include() {
        let store = test_store();

        let app_id = FileId::generate("src/app.c");
        let helper_id = FileId::generate("src/dir/helper.h");

        store
            .upsert_file(&FileInfo {
                file_id: app_id,
                path: "src/app.c".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file(&FileInfo {
                file_id: helper_id,
                path: "src/dir/helper.h".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        // `#include "dir/helper.h"` from src/app.c resolves to src/dir/helper.h
        let import = ImportDef {
            id: ImportId::generate(&app_id, "include", "dir/helper.h", None, 0),
            file_id: app_id,
            kind: ImportKind::Include,
            module: "dir/helper.h".into(),
            imported_name: String::new(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: true,
            range: Default::default(),
        };

        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id: app_id,
                    path: "src/app.c".into(),
                    language: Language::C,
                    content_hash: "abc".into(),
                    status: ParseStatus::Success,
                },
                imports: vec![import],
                ..Default::default()
            })
            .unwrap();

        let dependents = store.find_dependents_by_file(&helper_id).unwrap();
        assert!(
            dependents.iter().any(|(path, _mod)| path == "src/app.c"),
            "expected src/app.c in dependents of src/dir/helper.h, got: {dependents:?}"
        );
    }

    /// Regression: upsert_resolution_symbols must sync files.content_hash
    /// when it differs from the DB record, so has_complete_layer() sees
    /// matching hashes and recognises the layer as complete.
    #[test]
    fn test_upsert_resolution_symbols_updates_content_hash() {
        let store = test_store();
        let file_id = FileId::generate("src/main.c");

        // Register file with "old_hash"
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "src/main.c".into(),
                language: Language::C,
                content_hash: "old_hash".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        // Create FileFacts with a different content_hash
        let facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "src/main.c".into(),
                language: Language::C,
                content_hash: "new_hash".into(),
                status: ParseStatus::Success,
            },
            ..Default::default()
        };

        store.upsert_resolution_symbols(&file_id, &facts).unwrap();

        // Assert files.content_hash was updated
        let file_info = store.get_file(&file_id).unwrap().unwrap();
        assert_eq!(
            file_info.content_hash, "new_hash",
            "files.content_hash should be synced to the new hash"
        );

        // Assert file-level extraction state has the new hash and complete status
        let layer = store
            .get_file_extraction_state(&file_id, "resolution_symbols")
            .unwrap()
            .expect("resolution_symbols layer should exist");
        assert_eq!(layer.0, "complete", "layer status should be complete");
        assert_eq!(
            layer.1, "new_hash",
            "layer content_hash should match the new hash"
        );
    }

    #[test]
    fn missing_file_extraction_state_returns_none() {
        let store = test_store();
        let file_id = FileId::generate("src/missing.c");

        assert_eq!(
            store
                .get_file_extraction_state(&file_id, "structural")
                .unwrap(),
            None
        );
    }

    #[test]
    fn stale_call_owner_detection_ignores_global_calls() {
        let store = test_store();
        let file_id = FileId::generate("src/test.c");
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "src/test.c".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        let mut function = test_symbol(file_id, "run", SymbolKind::Function);
        function.range.start_byte = 100;
        function.range.end_byte = 200;
        let mut fallback = test_symbol(file_id, "global_state", SymbolKind::Struct);
        fallback.range.start_byte = 10;
        fallback.range.end_byte = 20;
        store
            .insert_symbols(&[function.clone(), fallback.clone()])
            .unwrap();

        let call_reference = |start_byte| {
            let range = TextRange {
                start_byte,
                end_byte: start_byte + 6,
                ..Default::default()
            };
            ReferenceUse {
                id: ReferenceId::generate(
                    &file_id,
                    Some(&fallback.id),
                    range.start_byte,
                    range.end_byte,
                    "helper",
                    ReferenceKind::Call,
                ),
                file_id,
                source_symbol: Some(fallback.id),
                scope_id: None,
                kind: ReferenceKind::Call,
                text: "helper".into(),
                name: "helper".into(),
                receiver: None,
                arity: None,
                range,
                binding_id: None,
                resolved: None,
            }
        };

        store.insert_references(&[call_reference(300)]).unwrap();
        assert!(
            !store
                .file_has_non_callable_call_reference_sources(&file_id)
                .unwrap(),
            "a global macro call has no callable owner and must not trigger rebuilding"
        );

        store.insert_references(&[call_reference(150)]).unwrap();
        assert!(
            store
                .file_has_non_callable_call_reference_sources(&file_id)
                .unwrap(),
            "a call inside a function owned by a struct is stale"
        );
    }

    // ── derive_capability_for_files tests ────────────────────────────────

    #[test]
    fn derive_capability_empty_store_returns_empty_mask() {
        let store = test_store();
        let file_id = FileId::generate("src/nonexistent.ts");
        let mask = store.derive_capability_for_files(&[file_id]);
        assert!(mask.is_zero(), "empty store should return empty mask");
    }

    #[test]
    fn derive_capability_empty_file_ids_returns_empty_mask() {
        let store = test_store();
        let mask = store.derive_capability_for_files(&[]);
        assert!(mask.is_zero(), "empty file_ids should return empty mask");
    }

    #[test]
    fn derive_capability_manifest_only_returns_manifest_bit() {
        let store = test_store();
        let file_id = FileId::generate("src/example.ts");
        let file = FileInfo {
            file_id,
            path: "src/example.ts".into(),
            language: Language::TypeScript,
            content_hash: "abc".into(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file).unwrap();

        // Insert manifest extraction state
        store
            .upsert_file_extraction_state(
                &file_id,
                "manifest",
                "abc",
                "complete",
                FactCoverage::from_bits(FactCoverage::MANIFEST),
            )
            .unwrap();

        let mask = store.derive_capability_for_files(&[file_id]);
        assert!(
            mask.has(FactCoverage::MANIFEST),
            "should have MANIFEST bit: {mask:?}"
        );
        assert!(
            !mask.has(FactCoverage::STRUCTURAL),
            "should NOT have STRUCTURAL bit with only manifest data: {mask:?}"
        );
        assert!(
            !mask.has(FactCoverage::CALL_EDGES),
            "should NOT have CALL_EDGES bit with only manifest data: {mask:?}"
        );
    }

    #[test]
    fn derive_capability_structural_no_edges_returns_manifest_and_structural() {
        let store = test_store();
        let file_id = FileId::generate("src/example.ts");
        let file = FileInfo {
            file_id,
            path: "src/example.ts".into(),
            language: Language::TypeScript,
            content_hash: "abc".into(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file).unwrap();

        // Insert structural extraction state (implies manifest is also present)
        store
            .upsert_file_extraction_state(
                &file_id,
                "structural",
                "abc",
                "complete",
                FactCoverage::from_bits(FactCoverage::MANIFEST | FactCoverage::STRUCTURAL),
            )
            .unwrap();

        let mask = store.derive_capability_for_files(&[file_id]);
        assert!(mask.has(FactCoverage::MANIFEST));
        assert!(mask.has(FactCoverage::STRUCTURAL));
        assert!(
            !mask.has(FactCoverage::CALL_EDGES),
            "should NOT have CALL_EDGES with no edges in store"
        );
    }

    #[test]
    fn read_catalog_tier_treats_dataflow_layer_as_full_index() {
        let store = test_store();
        let file_id = FileId::generate("src/example.ts");
        let file = FileInfo {
            file_id,
            path: "src/example.ts".into(),
            language: Language::TypeScript,
            content_hash: "abc".into(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file).unwrap();

        // The full extraction pipeline records the file-level layer as
        // `dataflow`; it does not also write a separate structural row.
        store
            .upsert_file_extraction_state(
                &file_id,
                "dataflow",
                "abc",
                "complete",
                FactCoverage::from_layers(&["dataflow"]),
            )
            .unwrap();

        assert_eq!(store.read_catalog_tier().unwrap(), "full");
    }

    #[test]
    fn read_catalog_tier_treats_mixed_dataflow_as_partial_structural() {
        let store = test_store();
        let file_a = FileId::generate("src/a.ts");
        let file_b = FileId::generate("src/b.ts");
        for (file_id, path) in [(file_a, "src/a.ts"), (file_b, "src/b.ts")] {
            store
                .upsert_file(&FileInfo {
                    file_id,
                    path: path.into(),
                    language: Language::TypeScript,
                    content_hash: "abc".into(),
                    status: ParseStatus::Success,
                })
                .unwrap();
        }
        store
            .upsert_file_extraction_state(
                &file_a,
                "dataflow",
                "abc",
                "complete",
                FactCoverage::from_layers(&["dataflow"]),
            )
            .unwrap();
        store
            .upsert_file_extraction_state(
                &file_b,
                "manifest",
                "abc",
                "complete",
                FactCoverage::from_layers(&["manifest"]),
            )
            .unwrap();

        assert_eq!(store.read_catalog_tier().unwrap(), "partial_structural");
    }

    #[test]
    fn unit_capability_mask_aggregates_every_derived_layer() {
        let store = test_store();
        let file = test_file();
        store.upsert_file(&file).unwrap();
        let unit_id = [7u8; 16];

        for (layer, bit) in [
            ("dataflow", FactCoverage::DATAFLOW),
            ("cfg", FactCoverage::CFG),
        ] {
            store
                .upsert_unit_extraction_state(&UnitExtractionStateRecord {
                    file_id: file.file_id,
                    unit_id,
                    layer: layer.into(),
                    content_hash: file.content_hash.clone(),
                    status: "complete".into(),
                    node_count: None,
                    edge_count: None,
                    budget_exceeded: false,
                    capability_mask: FactCoverage::from_bits(bit),
                    built_at: String::new(),
                })
                .unwrap();
        }

        let mask = store
            .get_capability_mask_for_unit(&file.file_id, &unit_id)
            .unwrap();
        assert!(mask.has(FactCoverage::DATAFLOW));
        assert!(mask.has(FactCoverage::CFG));
        assert!(
            store.get_capability_mask(&file.file_id).unwrap().is_zero(),
            "unit materialization must not promote the whole file"
        );
    }

    #[test]
    fn file_capability_mask_excludes_stale_layers() {
        let store = test_store();
        let mut file = test_file();
        store.upsert_file(&file).unwrap();
        store
            .upsert_file_extraction_state(
                &file.file_id,
                "dataflow",
                &file.content_hash,
                "complete",
                FactCoverage::from_bits(FactCoverage::DATAFLOW),
            )
            .unwrap();
        assert!(
            store
                .get_capability_mask(&file.file_id)
                .unwrap()
                .has(FactCoverage::DATAFLOW)
        );

        file.content_hash = "new-content".into();
        store.upsert_file(&file).unwrap();
        assert!(store.get_capability_mask(&file.file_id).unwrap().is_zero());
    }

    #[test]
    fn derive_capability_with_edges_returns_call_edges_bit() {
        let store = test_store();
        let file_id = FileId::generate("src/example.ts");

        // Insert file
        let file = FileInfo {
            file_id,
            path: "src/example.ts".into(),
            language: Language::TypeScript,
            content_hash: "abc".into(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file).unwrap();

        // Insert structural extraction state
        store
            .upsert_file_extraction_state(
                &file_id,
                "structural",
                "abc",
                "complete",
                FactCoverage::from_bits(FactCoverage::MANIFEST | FactCoverage::STRUCTURAL),
            )
            .unwrap();

        // Insert two symbols
        let caller = SymbolDef {
            id: SymbolId::generate(&file_id, "typescript", "caller.func", "function", None),
            kind: SymbolKind::Function,
            name: "func".into(),
            qualified_name: "caller.func".into(),
            symbol_path: vec!["caller".into(), "func".into()],
            file_id,
            language: Language::TypeScript,
            range: Default::default(),
            name_range: Default::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        let callee = SymbolDef {
            id: SymbolId::generate(&file_id, "typescript", "callee.helper", "function", None),
            kind: SymbolKind::Function,
            name: "helper".into(),
            qualified_name: "callee.helper".into(),
            symbol_path: vec!["callee".into(), "helper".into()],
            file_id,
            language: Language::TypeScript,
            range: Default::default(),
            name_range: Default::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store
            .insert_symbols(&[caller.clone(), callee.clone()])
            .unwrap();

        // Insert a call edge: caller -> callee
        let edge_id = EdgeId::generate(
            &caller.id,
            &callee.id,
            EdgeKind::Calls.as_str(),
            None,
            Provenance::TreeSitter.as_str(),
        );
        let edge = RawEdge::new(
            edge_id,
            caller.id,
            callee.id,
            EdgeKind::Calls,
            Confidence::certain(),
            Provenance::TreeSitter,
        );
        store.insert_edges(&[edge]).unwrap();

        let mask = store.derive_capability_for_files(&[file_id]);
        assert!(mask.has(FactCoverage::MANIFEST));
        assert!(mask.has(FactCoverage::STRUCTURAL));
        assert!(
            mask.has(FactCoverage::CALL_EDGES),
            "should have CALL_EDGES when edges exist in store: {mask:?}"
        );
        assert!(
            !mask.has(FactCoverage::CFG),
            "CFG not built by lazy structural"
        );
        assert!(
            !mask.has(FactCoverage::DATAFLOW),
            "DATAFLOW not built by lazy structural"
        );
        assert!(
            !mask.has(FactCoverage::SUMMARIES),
            "SUMMARIES not built by lazy structural"
        );
    }

    #[test]
    fn derive_capability_multiple_files_aggregates_correctly() {
        let store = test_store();

        // File A: manifest only
        let file_a = FileId::generate("src/a.ts");
        let info_a = FileInfo {
            file_id: file_a,
            path: "src/a.ts".into(),
            language: Language::TypeScript,
            content_hash: "hash_a".into(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&info_a).unwrap();
        store
            .upsert_file_extraction_state(
                &file_a,
                "manifest",
                "hash_a",
                "complete",
                FactCoverage::from_bits(FactCoverage::MANIFEST),
            )
            .unwrap();

        // File B: structural + edges
        let file_b = FileId::generate("src/b.ts");
        let info_b = FileInfo {
            file_id: file_b,
            path: "src/b.ts".into(),
            language: Language::TypeScript,
            content_hash: "hash_b".into(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&info_b).unwrap();
        store
            .upsert_file_extraction_state(
                &file_b,
                "structural",
                "hash_b",
                "complete",
                FactCoverage::from_bits(FactCoverage::MANIFEST | FactCoverage::STRUCTURAL),
            )
            .unwrap();

        let sym_b = SymbolDef {
            id: SymbolId::generate(&file_b, "typescript", "b.Foo.fn", "function", None),
            kind: SymbolKind::Function,
            name: "fn".into(),
            qualified_name: "b.Foo.fn".into(),
            symbol_path: vec!["b".into(), "Foo".into(), "fn".into()],
            file_id: file_b,
            language: Language::TypeScript,
            range: Default::default(),
            name_range: Default::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store.insert_symbols(std::slice::from_ref(&sym_b)).unwrap();

        // Self-edge (symbol calls itself) — verifies the query handles same source+target
        let edge_id = EdgeId::generate(
            &sym_b.id,
            &sym_b.id,
            EdgeKind::Calls.as_str(),
            None,
            Provenance::TreeSitter.as_str(),
        );
        let edge = RawEdge::new(
            edge_id,
            sym_b.id,
            sym_b.id,
            EdgeKind::Calls,
            Confidence::certain(),
            Provenance::TreeSitter,
        );
        store.insert_edges(&[edge]).unwrap();

        let mask = store.derive_capability_for_files(&[file_a, file_b]);
        assert!(
            mask.has(FactCoverage::MANIFEST),
            "aggregate should have MANIFEST from file_a"
        );
        assert!(
            mask.has(FactCoverage::STRUCTURAL),
            "aggregate should have STRUCTURAL from file_b"
        );
        assert!(
            mask.has(FactCoverage::CALL_EDGES),
            "aggregate should have CALL_EDGES from file_b edges"
        );
    }

    #[test]
    fn derive_capability_reads_dataflow_and_summaries_from_capability_mask() {
        let store = test_store();
        let file_id = FileId::generate("src/example.ts");
        let file = FileInfo {
            file_id,
            path: "src/example.ts".into(),
            language: Language::TypeScript,
            content_hash: "abc".into(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file).unwrap();

        // Simulate a full-index run: dataflow layer with DATAFLOW bit set
        store
            .upsert_file_extraction_state(
                &file_id,
                "dataflow",
                "abc",
                "complete",
                FactCoverage::from_layers(&["dataflow"]),
            )
            .unwrap();

        // Simulate summaries extraction
        store
            .upsert_file_extraction_state(
                &file_id,
                "summaries",
                "abc",
                "complete",
                FactCoverage::new(FactCoverage::SUMMARIES),
            )
            .unwrap();

        let mask = store.derive_capability_for_files(&[file_id]);
        assert!(
            mask.has(FactCoverage::DATAFLOW),
            "should have DATAFLOW from capability_mask: {mask:?}"
        );
        assert!(
            mask.has(FactCoverage::SUMMARIES),
            "should have SUMMARIES from capability_mask: {mask:?}"
        );
        // dataflow layer implies lower bits via from_layers too
        assert!(mask.has(FactCoverage::MANIFEST));
        assert!(mask.has(FactCoverage::STRUCTURAL));
        assert!(mask.has(FactCoverage::CALL_EDGES));
    }

    #[test]
    fn derive_capability_structural_only_no_dataflow() {
        let store = test_store();
        let file_id = FileId::generate("src/example.ts");
        let file = FileInfo {
            file_id,
            path: "src/example.ts".into(),
            language: Language::TypeScript,
            content_hash: "abc".into(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file).unwrap();

        // Lazy structural extraction: writes capability_mask=0, relies on layer fallback
        store
            .upsert_file_extraction_state(
                &file_id,
                "structural",
                "abc",
                "complete",
                FactCoverage::default(),
            )
            .unwrap();

        let mask = store.derive_capability_for_files(&[file_id]);
        assert!(mask.has(FactCoverage::MANIFEST));
        assert!(mask.has(FactCoverage::STRUCTURAL));
        assert!(
            !mask.has(FactCoverage::DATAFLOW),
            "should NOT have DATAFLOW with only structural layer: {mask:?}"
        );
        assert!(
            !mask.has(FactCoverage::CFG),
            "should NOT have CFG with only structural layer: {mask:?}"
        );
        assert!(
            !mask.has(FactCoverage::SUMMARIES),
            "should NOT have SUMMARIES with only structural layer: {mask:?}"
        );
    }

    #[test]
    fn derive_capability_excludes_stale_content_hash() {
        let store = test_store();
        let file_id = FileId::generate("src/example.ts");

        // File record with current hash "new_hash"
        let file = FileInfo {
            file_id,
            path: "src/example.ts".into(),
            language: Language::TypeScript,
            content_hash: "new_hash".into(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file).unwrap();

        // Stale extraction_state row with old hash "old_hash" — should be excluded
        store
            .upsert_file_extraction_state(
                &file_id,
                "dataflow",
                "old_hash",
                "complete",
                FactCoverage::from_layers(&["dataflow"]),
            )
            .unwrap();

        // Fresh structural row
        store
            .upsert_file_extraction_state(
                &file_id,
                "structural",
                "new_hash",
                "complete",
                FactCoverage::default(),
            )
            .unwrap();

        let mask = store.derive_capability_for_files(&[file_id]);
        // The stale row's content_hash doesn't match files.content_hash,
        // so its DATAFLOW capability should NOT appear.
        assert!(
            !mask.has(FactCoverage::DATAFLOW),
            "stale content_hash should be excluded, got DATAFLOW: {mask:?}"
        );
        // The fresh structural row should still contribute via layer fallback
        assert!(mask.has(FactCoverage::MANIFEST));
        assert!(mask.has(FactCoverage::STRUCTURAL));
    }

    // ── Bulk schema management tests ─────────────────────────────────────

    #[cfg(test)]
    mod bulk_tests {
        use super::*;
        use std::sync::Arc;

        fn test_store() -> Store {
            let store = Store::open_in_memory().unwrap();
            store.init_schema().unwrap();
            store
        }

        fn list_indexes(store: &Store) -> Vec<String> {
            let conn = store.lock();
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='index' ORDER BY name")
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        }

        fn index_exists(store: &Store, name: &str) -> bool {
            list_indexes(store).contains(&name.to_string())
        }

        fn index_sql(store: &Store, name: &str) -> String {
            let conn = store.lock();
            conn.query_row(
                "SELECT sql FROM sqlite_master WHERE type='index' AND name = ?1",
                [name],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
        }

        #[test]
        fn full_rebuild_guard_drop_repairs_schema() {
            let store = Arc::new(test_store());

            // Drop one index to simulate partial schema
            {
                let conn = store.lock();
                conn.execute_batch("DROP INDEX IF EXISTS idx_symbols_qname")
                    .unwrap();
            }
            assert!(
                !index_exists(&store, "idx_symbols_qname"),
                "index should be dropped"
            );

            // Guard should recreate the missing index on drop
            {
                let _guard = FullRebuildGuard::new(&store);
                // guard goes out of scope here — triggers repair
            }

            assert!(
                index_exists(&store, "idx_symbols_qname"),
                "guard drop should recreate missing index"
            );
        }

        #[test]
        fn full_rebuild_guard_commit_skips_repair() {
            let store = Arc::new(test_store());

            // Drop one index
            {
                let conn = store.lock();
                conn.execute_batch("DROP INDEX IF EXISTS idx_symbols_qname")
                    .unwrap();
            }
            assert!(!index_exists(&store, "idx_symbols_qname"));

            // Commit the guard — should NOT repair on drop
            {
                let guard = FullRebuildGuard::new(&store);
                guard.commit();
            }

            assert!(
                !index_exists(&store, "idx_symbols_qname"),
                "committed guard should NOT repair schema"
            );
        }

        #[test]
        fn drop_writable_indexes_removes_all() {
            let store = test_store();

            // Sanity: init_schema created many indexes
            let before = list_indexes(&store);
            assert!(
                before.iter().any(|n| n.starts_with("idx_")),
                "init_schema should create indexes"
            );

            let mut rebuild = false;
            store.drop_writable_indexes(&mut rebuild).unwrap();
            assert!(rebuild, "must_rebuild should be set to true");

            // After drop_writable_indexes, only PK autoindexes and
            // extraction/summary indexes (not in ALL_WRITE_INDEXES) should remain.
            let after = list_indexes(&store);
            let dropped: Vec<&str> = crate::bulk_schema::ALL_WRITE_INDEXES.to_vec();
            for name in &dropped {
                assert!(
                    !after.contains(&name.to_string()),
                    "index {name} should have been dropped"
                );
            }

            // Extraction and summary indexes should still exist
            for name in crate::bulk_schema::EXTRACTION_AND_SUMMARY_INDEXES {
                assert!(
                    after.contains(&name.to_string()),
                    "kept index {name} should still exist"
                );
            }
        }

        #[test]
        fn create_resolution_indexes_creates_correct_set() {
            let store = test_store();

            // Drop everything first
            let mut rebuild = false;
            store.drop_writable_indexes(&mut rebuild).unwrap();

            // Create resolution-only indexes
            store.create_resolution_indexes().unwrap();

            for name in crate::bulk_schema::RESOLUTION_INDEXES {
                assert!(
                    index_exists(&store, name),
                    "resolution index {name} should exist"
                );
            }

            // Non-resolution indexes should NOT exist
            for name in crate::bulk_schema::FINAL_QUERY_INDEXES {
                assert!(
                    !index_exists(&store, name),
                    "final index {name} should NOT exist yet"
                );
            }
        }

        #[test]
        fn ensure_required_schema_objects_fixes_missing_index() {
            let store = test_store();

            // Drop one index
            {
                let conn = store.lock();
                conn.execute_batch("DROP INDEX IF EXISTS idx_symbols_kind")
                    .unwrap();
            }
            assert!(
                !index_exists(&store, "idx_symbols_kind"),
                "index should be dropped"
            );

            store.ensure_required_schema_objects().unwrap();

            assert!(
                index_exists(&store, "idx_symbols_kind"),
                "ensure_required should recreate missing index"
            );
        }

        #[test]
        fn ensure_required_schema_objects_repairs_extraction_job_active_indexes() {
            let store = test_store();

            {
                let conn = store.lock();
                conn.execute_batch(
                    "DROP INDEX IF EXISTS idx_extraction_jobs_active_file_layer;
                     DROP INDEX IF EXISTS idx_extraction_jobs_active_unit_layer;",
                )
                .unwrap();
            }

            let repaired = store.ensure_required_schema_objects().unwrap();
            assert_eq!(repaired, 2, "two missing extraction job indexes repaired");

            let file_sql = index_sql(&store, "idx_extraction_jobs_active_file_layer");
            assert!(
                file_sql.contains("unit_id IS NULL")
                    && file_sql.contains("status IN ('queued', 'building')"),
                "file-level active job index must match schema predicate, got: {file_sql}"
            );
            assert!(
                !file_sql.contains("status = 'active'"),
                "old active-status predicate must not be recreated: {file_sql}"
            );

            let unit_sql = index_sql(&store, "idx_extraction_jobs_active_unit_layer");
            assert!(
                unit_sql.contains("unit_id IS NOT NULL")
                    && unit_sql.contains("status IN ('queued', 'building')"),
                "unit-level active job index must match schema predicate, got: {unit_sql}"
            );
            assert!(
                !unit_sql.contains("status = 'active'"),
                "old active-status predicate must not be recreated: {unit_sql}"
            );
        }

        #[test]
        fn ensure_required_schema_objects_noop_when_all_present() {
            let store = test_store();

            // Should succeed without errors when nothing is missing
            store.ensure_required_schema_objects().unwrap();

            // All expected indexes should still exist
            for name in crate::bulk_schema::ALL_WRITE_INDEXES {
                assert!(
                    index_exists(&store, name),
                    "index {name} should still exist"
                );
            }
        }

        #[test]
        fn ensure_required_schema_objects_returns_count() {
            let store = test_store();

            // When everything is present, 0 objects are repaired.
            let repaired = store.ensure_required_schema_objects().unwrap();
            assert_eq!(repaired, 0, "fresh schema should require no repair");

            // Drop one index — now exactly 1 object is missing.
            {
                let conn = store.lock();
                conn.execute_batch("DROP INDEX IF EXISTS idx_symbols_kind")
                    .unwrap();
            }
            let repaired = store.ensure_required_schema_objects().unwrap();
            assert_eq!(
                repaired, 1,
                "one missing index should be reported as single repair"
            );
        }

        #[test]
        fn create_final_indexes_and_rebuild_fts_works() {
            let store = test_store();

            // Drop all writable indexes first
            let mut rebuild = false;
            store.drop_writable_indexes(&mut rebuild).unwrap();

            // Create final indexes + rebuild FTS
            store.create_final_indexes_and_rebuild_fts().unwrap();

            // All final query indexes should exist
            for name in crate::bulk_schema::FINAL_QUERY_INDEXES {
                assert!(
                    index_exists(&store, name),
                    "final index {name} should exist after create_final_indexes_and_rebuild_fts"
                );
            }
        }
    }
}
