//! Store lifecycle: database open, schema init, cross-process locking.

use crate::schema::SCHEMA_DDL;
use crate::store_fts::{chrono_now_ms, is_process_alive};

use rusqlite::{params, Connection, OpenFlags};
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

    /// Open an existing SQLite database read-only.
    ///
    /// This is intended for status/probing paths that must not create or modify
    /// a candidate database. Mutation methods on the returned store will fail at
    /// the SQLite layer because the underlying connection is read-only.
    pub fn open_db_read_only(db_path: &Path) -> anyhow::Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY;
        let conn = Connection::open_with_flags(db_path, flags)?;
        conn.execute_batch(
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
                read_conn: None,
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
    /// Creates all tables if they don't exist. Safe to call multiple times —
    /// all DDL uses `CREATE TABLE IF NOT EXISTS`.
    pub fn init_schema(&self) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute_batch(SCHEMA_DDL)?;

        // Migration: add callee_name to cfg_nodes (nullable, backward-compat)
        {
            let has_col: bool = conn
                .prepare("SELECT callee_name FROM cfg_nodes LIMIT 0")
                .is_ok();
            if !has_col {
                conn.execute_batch("ALTER TABLE cfg_nodes ADD COLUMN callee_name TEXT;")?;
            }
        }

        // Migration: add call_context to cfg_nodes (nullable, backward-compat)
        {
            let has_col: bool = conn
                .prepare("SELECT call_context FROM cfg_nodes LIMIT 0")
                .is_ok();
            if !has_col {
                conn.execute_batch("ALTER TABLE cfg_nodes ADD COLUMN call_context TEXT;")?;
            }
        }

        Ok(())
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

        let existing: Option<(i64, i64)> = match tx.query_row(
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
        ) {
            Ok(existing) => existing,
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                tracing::warn!(?e, "Failed to query exclusive lock PID");
                None
            }
        };

        if let Some((existing_pid, _ts)) = existing {
            if existing_pid != pid as i64 && is_process_alive(existing_pid) {
                anyhow::bail!("Another atlas process (PID {existing_pid}) already holds the lock");
            }
            // Stale lock — steal it: replace old entry
            tx.execute(
                "DELETE FROM project_metadata WHERE key = 'exclusive_lock_pid'",
                [],
            )?;
        }

        let lock_value = format!("{pid}:{now}");
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
        let existing: Option<i64> = match conn.query_row(
            "SELECT value FROM project_metadata WHERE key = 'exclusive_lock_pid'",
            [],
            |row| {
                let v: String = row.get(0)?;
                Ok(v.split(':').next().and_then(|s| {
                    s.parse()
                        .map_err(|e| {
                            tracing::warn!(?e, pid_value = %v, "Failed to parse lock PID from metadata");
                            e
                        })
                        .ok()
                }))
            },
        ) {
            Ok(existing) => existing,
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                tracing::warn!(?e, "Failed to query exclusive lock PID for release");
                None
            }
        };

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
