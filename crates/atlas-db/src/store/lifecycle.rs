//! Store lifecycle: database open, schema init, cross-process locking.

use crate::schema::{CURRENT_SCHEMA_VERSION, SCHEMA_DDL};
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
    /// timeout, ~20 MB cache) are applied on open.
    pub fn open_db(db_path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(db_path)?;

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
            reader: StoreReader {
                conn: Mutex::new(conn),
            },
            db_path: db_path.to_path_buf(),
        })
    }

    /// Open in-memory (for tests).
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self {
            reader: StoreReader {
                conn: Mutex::new(conn),
            },
            db_path: PathBuf::from(":memory:"),
        })
    }

    /// Initialize the schema (idempotent).
    pub fn init_schema(&self) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute_batch(SCHEMA_DDL)?;

        // Record current version (always V1 during rapid development)
        conn.execute(
            "INSERT OR IGNORE INTO schema_versions (version, description)
             VALUES (?1, ?2)",
            params![CURRENT_SCHEMA_VERSION, "v2: add arg_index to data_nodes"],
        )?;

        Ok(())
    }

    // ── Exclusive lock (cross-process, via project_metadata table) ─────────

    /// Try to acquire an exclusive write lock.
    ///
    /// Records the current PID and timestamp in `project_metadata`.
    /// Fails if another process already holds the lock and is still alive.
    /// Stale locks (process died) are automatically stolen.
    pub fn acquire_exclusive_lock(&self) -> anyhow::Result<()> {
        let conn = self.lock();
        let pid = std::process::id();
        let now = chrono_now_ms();

        // Check for existing lock
        let existing: Option<(i64, i64)> = conn
            .query_row(
                "SELECT value FROM project_metadata WHERE key = 'exclusive_lock_pid'",
                [],
                |row| {
                    let v: String = row.get(0)?;
                    // Format: "pid:timestamp_ms"
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
            // Stale lock — steal it
        }

        // Write our lock
        let lock_value = format!("{}:{}", pid, now);
        conn.execute(
            "INSERT OR REPLACE INTO project_metadata (key, value) VALUES ('exclusive_lock_pid', ?1)",
            params![lock_value],
        )?;
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
