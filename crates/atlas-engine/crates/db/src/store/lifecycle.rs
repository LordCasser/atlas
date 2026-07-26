//! Store lifecycle: database open, schema init, cross-process locking.

use crate::schema::{CURRENT_SCHEMA_VERSION, SCHEMA_DDL};
use crate::store_fts::{chrono_now_ms, is_process_alive};

use rusqlite::{Connection, OpenFlags, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{Store, StoreReader};

// ── Exclusive lock rejection (shared by filesync + focus_materialize) ───────

/// Structured rejection when Focus/MCP must not write because CLI holds the lock.
///
/// Single source for diagnostic code + reason + suggested_action (Task 2 DRY).
#[derive(Debug, Clone)]
pub struct ExclusiveLockHeld {
    pub holder_pid: u32,
    pub code: &'static str,
    pub reason: String,
    pub suggested_action: String,
}

impl std::fmt::Display for ExclusiveLockHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}] {} (holder_pid={}). {}",
            self.code, self.reason, self.holder_pid, self.suggested_action
        )
    }
}

impl std::error::Error for ExclusiveLockHeld {}

// ── Generation tracking ─────────────────────────────────────────────────────

/// Analysis mode for generation tracking.
///
/// Identifies which resolution/graph pipeline phase is active. The hash of
/// mode + path aliases is stored alongside the generation counter so the
/// pipeline can detect configuration changes without re-reading every file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineGrade {
    Manifest,
    Structural,
    Full,
}

impl PipelineGrade {
    pub fn as_str(&self) -> &'static str {
        match self {
            PipelineGrade::Manifest => "manifest",
            PipelineGrade::Structural => "structural",
            PipelineGrade::Full => "full",
        }
    }
}

/// Key for the resolution generation counter in project_metadata.
pub const KEY_RESOLUTION_GENERATION: &str = "resolution_generation_version";
/// Key for the resolution config hash in project_metadata.
pub const KEY_RESOLUTION_CONFIG_HASH: &str = "resolution_config_hash";
/// Key for the graph generation counter in project_metadata.
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

        // Open a pool of dedicated read connections.  query_only = ON ensures
        // accidental writes through these connections fail at the SQLite
        // level instead of silently corrupting data.  Multiple connections
        // let parallel readers (rayon resolution workers) proceed without
        // serializing behind one mutex.
        let mut read_pool = Vec::with_capacity(crate::store::read_pool_size());
        for _ in 0..crate::store::read_pool_size() {
            let rc = Connection::open(db_path)?;
            rc.execute_batch(
                r#"
                PRAGMA query_only = ON;
                PRAGMA busy_timeout = 10000;
                PRAGMA cache_size = -65536;
                PRAGMA temp_store = MEMORY;
                PRAGMA mmap_size = 268435456;
                "#,
            )?;
            read_pool.push(Mutex::new(rc));
        }

        Ok(Self {
            reader: StoreReader {
                conn: Mutex::new(conn),
                read_pool,
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
                read_pool: Vec::new(),
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
                read_pool: Vec::new(),
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
        validate_schema_version_for_init(&conn)?;
        conn.execute_batch(SCHEMA_DDL)?;
        conn.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)?;

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
        mode: &PipelineGrade,
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

    /// Reject Focus/MCP writes if another **live** process holds the exclusive lock.
    ///
    /// Single source for diagnostic code + reason + suggested_action (Task 2 DRY).
    pub fn reject_if_exclusive_lock_held_by_other(&self) -> Result<(), ExclusiveLockHeld> {
        match self.exclusive_lock_held_by_other() {
            Ok(Some(pid)) => Err(ExclusiveLockHeld {
                holder_pid: pid,
                code: "cli_index_lock_held",
                reason: format!(
                    "atlas.db is exclusively locked by another process (PID {pid}), \
                     typically `atlas index` or `atlas sync`"
                ),
                suggested_action:
                    "Stop the concurrent CLI index/sync process, then retry the query".into(),
            }),
            Ok(None) => Ok(()),
            Err(e) => Err(ExclusiveLockHeld {
                holder_pid: 0,
                code: "cli_index_lock_check_failed",
                reason: format!("could not read exclusive lock metadata: {e:#}"),
                suggested_action:
                    "Ensure the project database is readable and no index is running; then retry"
                        .into(),
            }),
        }
    }

    /// Returns `Some(pid)` if another **live** process holds the exclusive lock.
    ///
    /// Lightweight read (no lock acquisition). Used by Focus/MCP write paths to
    /// **reject** concurrent CLI index rather than wait.
    pub fn exclusive_lock_held_by_other(&self) -> anyhow::Result<Option<u32>> {
        let conn = self.lock_read();
        let pid = std::process::id();
        let existing: Option<(i64, i64)> = match conn.query_row(
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
                tracing::warn!(?e, "Failed to query exclusive lock PID (read)");
                None
            }
        };
        if let Some((existing_pid, _ts)) = existing {
            if existing_pid != pid as i64 && is_process_alive(existing_pid) {
                return Ok(Some(existing_pid as u32));
            }
        }
        Ok(None)
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

fn validate_schema_version_for_init(conn: &Connection) -> anyhow::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == CURRENT_SCHEMA_VERSION {
        return Ok(());
    }

    let user_table_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table'
           AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if version == 0 && user_table_count == 0 {
        return Ok(());
    }

    anyhow::bail!(
        "Atlas database schema version is v{version}, expected v{CURRENT_SCHEMA_VERSION}. Remove the project .atlas/atlas.db or .atlas/ directory and re-run atlas."
    );
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
    fn init_schema_sets_sqlite_user_version() {
        let store = test_store();
        let conn = store.lock_read();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();

        assert_eq!(version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn init_schema_rejects_nonempty_unversioned_database() {
        let store = Store::open_in_memory().unwrap();
        {
            let conn = store.lock();
            conn.execute("CREATE TABLE legacy_table (id INTEGER)", [])
                .unwrap();
        }

        let err = store
            .init_schema()
            .expect_err("non-empty unversioned database must not be migrated");

        assert!(
            err.to_string().contains("schema version is v0"),
            "error should name the incompatible schema version: {err:#}"
        );
        assert!(
            err.to_string().contains("re-run atlas"),
            "error should point to rebuild instead of migration: {err:#}"
        );
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
        let mode = PipelineGrade::Structural;
        let hash1 = store.resolution_config_hash(&mode, None).unwrap();
        let hash2 = store.resolution_config_hash(&mode, None).unwrap();
        assert_eq!(hash1, hash2, "same input must produce same hash");
    }

    #[test]
    fn test_resolution_config_hash_changes_with_mode() {
        let store = test_store();
        let h1 = store
            .resolution_config_hash(&PipelineGrade::Structural, None)
            .unwrap();
        let h2 = store
            .resolution_config_hash(&PipelineGrade::Full, None)
            .unwrap();
        assert_ne!(h1, h2, "different modes must produce different hashes");
    }

    #[test]
    fn test_resolution_config_hash_changes_with_alias() {
        let store = test_store();
        let mode = PipelineGrade::Structural;

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
