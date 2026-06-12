//! Store lifecycle: database open, schema init, cross-process locking.

use crate::schema::SCHEMA_DDL;
use crate::store_fts::{chrono_now_ms, is_process_alive};

use rusqlite::{Connection, OpenFlags, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{Store, StoreReader};

// ── Focus schema migration ──────────────────────────────────────────────────

/// Apply focus schema migration: add `closure_id` and `generation` columns
/// to `extraction_jobs`, and create the supporting index.
///
/// Uses `PRAGMA table_info` to check column existence before ALTER TABLE,
/// so repeated calls are idempotent without relying on error suppression.
fn apply_focus_schema_migration(conn: &Connection) -> anyhow::Result<()> {
    // Check existing columns on extraction_jobs
    let mut stmt = conn.prepare("PRAGMA table_info('extraction_jobs')")?;
    let columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))? // column name is at index 1
        .filter_map(|r| r.ok())
        .collect();

    if !columns.iter().any(|c| c == "closure_id") {
        conn.execute("ALTER TABLE extraction_jobs ADD COLUMN closure_id TEXT", [])?;
    }
    if !columns.iter().any(|c| c == "generation") {
        conn.execute("ALTER TABLE extraction_jobs ADD COLUMN generation INTEGER", [])?;
    }

    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_extraction_jobs_closure
             ON extraction_jobs(closure_id, generation);",
    )?;

    Ok(())
}

// ── Generation tracking ─────────────────────────────────────────────────────

/// Analysis mode for generation tracking.
///
/// Identifies which resolution/graph pipeline phase is active. The hash of
/// mode + path aliases is stored alongside the generation counter so the
/// pipeline can detect configuration changes without re-reading every file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexMode {
    Manifest,
    Structural,
    Full,
}

impl IndexMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            IndexMode::Manifest => "manifest",
            IndexMode::Structural => "structural",
            IndexMode::Full => "full",
        }
    }
}

/// Key for the resolution generation counter in project_metadata.
#[allow(dead_code)]
pub const KEY_RESOLUTION_GENERATION: &str = "resolution_generation_version";
/// Key for the resolution config hash in project_metadata.
#[allow(dead_code)]
pub const KEY_RESOLUTION_CONFIG_HASH: &str = "resolution_config_hash";
/// Key for the graph generation counter in project_metadata.
#[allow(dead_code)]
pub const KEY_GRAPH_GENERATION: &str = "graph_generation_version";

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

        // Migration: P3 per-file resolution fingerprint column.
        // New DBs get it from CREATE TABLE; existing DBs get it via ALTER TABLE.
        // Ignore error if column already exists (idempotent).
        let _ = conn.execute(
            "ALTER TABLE extraction_state ADD COLUMN resolution_fingerprint TEXT",
            [],
        );

        // Focus schema migration: extraction_jobs closure tracking columns.
        apply_focus_schema_migration(&conn)?;

        Ok(())
    }

    // ── Generation tracking ────────────────────────────────────────────────

    /// Get a generation counter value by key. Returns 0 if key doesn't exist.
    pub fn get_generation(&self, key: &str) -> anyhow::Result<u64> {
        let conn = self.lock_read();
        match conn.query_row(
            "SELECT value FROM project_metadata WHERE key = ?1",
            params![key],
            |row| {
                let v: String = row.get(0)?;
                Ok(v.parse::<u64>().unwrap_or(0))
            },
        ) {
            Ok(v) => Ok(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// Set a generation counter value by key.
    pub fn set_generation(&self, key: &str, version: u64) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO project_metadata (key, value) VALUES (?1, ?2)",
            params![key, version.to_string()],
        )?;
        Ok(())
    }

    /// Increment a generation counter by 1 and return the new value.
    /// Stores 1 if key doesn't exist.
    ///
    /// Uses a single `self.lock()` to atomically read + write.
    pub fn bump_generation(&self, key: &str) -> anyhow::Result<u64> {
        let conn = self.lock();
        let current: u64 = match conn.query_row(
            "SELECT value FROM project_metadata WHERE key = ?1",
            params![key],
            |row| {
                let v: String = row.get(0)?;
                Ok(v.parse::<u64>().unwrap_or(0))
            },
        ) {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => 0,
            Err(e) => return Err(e.into()),
        };
        let next = current + 1;
        conn.execute(
            "INSERT OR REPLACE INTO project_metadata (key, value) VALUES (?1, ?2)",
            params![key, next.to_string()],
        )?;
        Ok(next)
    }

    /// Compute a resolution config hash from analysis mode + path alias.
    ///
    /// Uses blake3 over `mode.as_str()` concatenated with sorted path alias
    /// entries. Returns a hex string. This detects when resolver configuration
    /// changes, not just file content changes.
    pub fn resolution_config_hash(
        &self,
        mode: &IndexMode,
        path_alias: Option<&HashMap<String, String>>,
    ) -> anyhow::Result<String> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(mode.as_str().as_bytes());

        if let Some(aliases) = path_alias {
            let mut entries: Vec<(&String, &String)> = aliases.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (k, v) in entries {
                hasher.update(k.as_bytes());
                hasher.update(b"=");
                hasher.update(v.as_bytes());
            }
        }

        Ok(hasher.finalize().to_hex().to_string())
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

// ── Generation tracking tests ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        store
    }

    #[test]
    fn test_get_generation_nonexistent_returns_zero() {
        let store = test_store();
        let v = store.get_generation("nonexistent.key").unwrap();
        assert_eq!(v, 0);
    }

    #[test]
    fn test_set_and_get_generation() {
        let store = test_store();
        store.set_generation("test.key", 42).unwrap();
        let v = store.get_generation("test.key").unwrap();
        assert_eq!(v, 42);
    }

    #[test]
    fn test_bump_generation_creates_and_increments() {
        let store = test_store();
        // Bump non-existent key → creates with value 1
        let v1 = store.bump_generation("counter").unwrap();
        assert_eq!(v1, 1);
        // Bump again → 2
        let v2 = store.bump_generation("counter").unwrap();
        assert_eq!(v2, 2);
    }

    #[test]
    fn test_resolution_config_hash_idempotent() {
        let store = test_store();
        let mode = IndexMode::Structural;
        let hash1 = store.resolution_config_hash(&mode, None).unwrap();
        let hash2 = store.resolution_config_hash(&mode, None).unwrap();
        assert_eq!(hash1, hash2, "same input must produce same hash");
    }

    #[test]
    fn test_resolution_config_hash_changes_with_mode() {
        let store = test_store();
        let h1 = store
            .resolution_config_hash(&IndexMode::Structural, None)
            .unwrap();
        let h2 = store
            .resolution_config_hash(&IndexMode::Full, None)
            .unwrap();
        assert_ne!(h1, h2, "different modes must produce different hashes");
    }

    #[test]
    fn test_resolution_config_hash_changes_with_alias() {
        let store = test_store();
        let mode = IndexMode::Structural;

        let mut aliases1 = HashMap::new();
        aliases1.insert("/src".to_string(), "/opt/src".to_string());

        let mut aliases2 = HashMap::new();
        aliases2.insert("/src".to_string(), "/other/src".to_string());

        let h1 = store
            .resolution_config_hash(&mode, Some(&aliases1))
            .unwrap();
        let h2 = store
            .resolution_config_hash(&mode, Some(&aliases2))
            .unwrap();
        assert_ne!(h1, h2, "different aliases must produce different hashes");
    }
}
