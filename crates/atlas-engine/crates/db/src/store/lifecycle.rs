//! Store lifecycle: database open, schema init, cross-process locking.

use crate::schema::{CURRENT_SCHEMA_VERSION, SCHEMA_DDL, SchemaStatus, check_and_migrate};
use crate::store_fts::{chrono_now_ms, is_process_alive};

use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{Store, StoreReader};

// ── Lifecycle ───────────────────────────────────────────────────────────────

impl Store {
    /// Open or create a SQLite database at the given file path.
    ///
    /// This is a low-level primitive: it does **not** create any directories,
    /// discover project roots, or validate the path. The caller is responsible
    /// for ensuring the parent directory exists (typically via
    /// `Workspace::ensure_atlas_dir`).
    ///
    /// Performance PRAGMAs (WAL journal, NORMAL sync, foreign keys, busy
    /// timeout, mmap, temp memory, ~64 MB cache) are applied on open.
    pub fn open_db(db_path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(db_path)?;

        // Performance pragmas
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 10000;
            PRAGMA cache_size = -65536; -- ~64 MB
            PRAGMA temp_store = MEMORY;
            PRAGMA mmap_size = 268435456; -- 256 MB
            "#,
        )?;

        // Open a dedicated read connection.  query_only = ON ensures
        // accidental writes through this connection fail at the SQLite
        // level instead of silently corrupting data.
        let read_conn = Connection::open(db_path)?;
        read_conn.execute_batch(
            r#"
            PRAGMA query_only = ON;
            PRAGMA busy_timeout = 10000;
            PRAGMA cache_size = -65536;
            PRAGMA temp_store = MEMORY;
            PRAGMA mmap_size = 268435456;
            "#,
        )?;

        Ok(Self {
            reader: StoreReader {
                conn: Mutex::new(conn),
                read_conn: Some(Mutex::new(read_conn)),
            },
            db_path: db_path.to_path_buf(),
        })
    }

    /// Open an empty in-memory database (no file on disk).
    ///
    /// Used by tests and by [`open_project`] with `storage: "memory"`.
    /// No WAL journal (single-connection, no concurrency).  The returned
    /// store is isolated to the current process and destroyed on drop.
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self {
            reader: StoreReader {
                conn: Mutex::new(conn),
                read_conn: None,
            },
            db_path: PathBuf::from(":memory:"),
        })
    }

    /// Initialize the schema (idempotent).
    ///
    /// On a fresh database creates all tables and records V1.
    /// On an existing database runs pending migrations via [`check_and_migrate`].
    /// Returns the schema status for the caller to report.
    pub fn init_schema(&self) -> anyhow::Result<SchemaStatus> {
        let conn = self.lock();
        conn.execute_batch(SCHEMA_DDL)?;

        // Run migration check — handles fresh, current, upgradable, and incompatible
        let status = check_and_migrate(&conn)?;

        // Record current version if schema_versions is empty (fresh DB)
        if matches!(status, SchemaStatus::Current) {
            let existing: i64 = conn
                .query_row("SELECT COUNT(*) FROM schema_versions", [], |r| r.get(0))
                .unwrap_or(0);
            if existing == 0 {
                conn.execute(
                    "INSERT INTO schema_versions (version, description)
                     VALUES (?1, ?2)",
                    params![CURRENT_SCHEMA_VERSION, "v1: initial schema"],
                )?;
            }
        }

        Ok(status)
    }

    // ── Exclusive lock (cross-process, via project_metadata table) ─────────

    /// Try to acquire an exclusive write lock (atomic via SQLite transaction).
    ///
    /// Uses BEGIN IMMEDIATE to atomically check for existing lock and record
    /// the current PID.  Fails immediately if another process holds the lock
    /// and is still alive.  Stale locks (process died) are stolen.
    pub fn acquire_exclusive_lock(&self) -> anyhow::Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let pid = std::process::id();
        let now = chrono_now_ms();

        let existing: Option<(i64, i64)> = tx
            .query_row(
                "SELECT value FROM project_metadata WHERE key = 'exclusive_lock_pid'",
                [],
                |row| {
                    let v: String = row.get(0)?;
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
            // Stale lock — steal it: replace old entry
            tx.execute(
                "DELETE FROM project_metadata WHERE key = 'exclusive_lock_pid'",
                [],
            )?;
        }

        let lock_value = format!("{}:{}", pid, now);
        tx.execute(
            "INSERT INTO project_metadata (key, value) VALUES ('exclusive_lock_pid', ?1)",
            params![lock_value],
        )?;
        tx.commit()?;
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
}
