//! File lock: prevents concurrent atlas processes from writing the same database.
//!
//! Uses a PID-based metadata row (`project_metadata.exclusive_lock_pid`) as the
//! locking mechanism. The lock value stores `{pid}:{timestamp_ms}`. A process can
//! re-acquire the lock because the check compares by PID — the same process is
//! allowed to take the lock. A stale lock (PID no longer alive) is silently stolen.
//!
//! The underlying acquire/release is atomic via a short-lived `BEGIN IMMEDIATE`
//! transaction, but the lock is **not** a held SQLite transaction — it is a
//! metadata row that persists after commit.
//!
//! ## Usage
//!
//! ```ignore
//! let guard = FileLock::acquire(&store)?;
//! // ... exclusive write access to the database ...
//! drop(guard); // metadata row deleted, lock released
//! ```

use db::Store;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// FileLockGuard
// ---------------------------------------------------------------------------

/// RAII guard that holds the exclusive lock metadata row.
/// On drop, deletes the `exclusive_lock_pid` metadata row (if still held by this PID).
pub struct FileLockGuard {
    store: Arc<Store>,
}

impl FileLockGuard {
    /// Manually release the lock (equivalent to dropping).
    pub fn release(self) {
        drop(self)
    }
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        // Delete the exclusive_lock_pid metadata row to release the lock.
        // Errors are logged but not propagated — by the time we drop,
        // the caller is done with the database anyway.
        if let Err(e) = self.store.release_exclusive_lock() {
            tracing::warn!("Failed to release exclusive lock: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// FileLock
// ---------------------------------------------------------------------------

/// Acquire an exclusive write lock on the Atlas database.
///
/// Uses a PID-based metadata row (`project_metadata.exclusive_lock_pid`)
/// wrapped in a short `BEGIN IMMEDIATE` transaction for atomic check-and-set.
/// The lock row persists after the transaction commits and is only deleted by
/// `release_exclusive_lock` when the guard drops.
///
/// Same-process re-acquisition is allowed (PID check passes); only a different
/// live process is blocked.
pub struct FileLock;

/// Structured rejection when Focus/MCP must not write because CLI holds the lock.
#[derive(Debug, Clone)]
pub struct IndexLockHeld {
    pub holder_pid: u32,
    pub code: &'static str,
    pub reason: String,
    pub suggested_action: String,
}

impl std::fmt::Display for IndexLockHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}] {} (holder_pid={}). {}",
            self.code, self.reason, self.holder_pid, self.suggested_action
        )
    }
}

impl std::error::Error for IndexLockHeld {}

impl FileLock {
    /// Acquire an exclusive lock. Fails immediately if another live process holds it.
    pub fn acquire(store: &Arc<Store>) -> anyhow::Result<FileLockGuard> {
        store.acquire_exclusive_lock()?;
        Ok(FileLockGuard {
            store: Arc::clone(store),
        })
    }

    /// Reject Focus/MCP writes if CLI (or another process) holds the exclusive lock.
    ///
    /// No wait/queue/retry — agent must stop `atlas index`/`sync` then retry.
    pub fn reject_if_held_by_other(store: &Store) -> Result<(), IndexLockHeld> {
        match store.exclusive_lock_held_by_other() {
            Ok(Some(pid)) => Err(IndexLockHeld {
                holder_pid: pid,
                code: "cli_index_lock_held",
                reason: format!(
                    "atlas.db is exclusively locked by another process (PID {pid}), \
                     typically `atlas index` or `atlas sync`"
                ),
                suggested_action: "Stop the concurrent CLI index/sync process, then retry the query"
                    .into(),
            }),
            Ok(None) => Ok(()),
            Err(e) => {
                // Fail closed on lock read errors so Focus never races a half-read lock row.
                Err(IndexLockHeld {
                    holder_pid: 0,
                    code: "cli_index_lock_check_failed",
                    reason: format!("could not read exclusive lock metadata: {e:#}"),
                    suggested_action:
                        "Ensure the project database is readable and no index is running; then retry"
                            .into(),
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_lock_acquire_release() {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let store = Arc::new(store);

        let guard = FileLock::acquire(&store).unwrap();
        drop(guard);
        // After release, another lock should be obtainable
        let _guard2 = FileLock::acquire(&store).unwrap();
    }

    #[test]
    fn test_file_lock_guard_release() {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let store = Arc::new(store);

        let guard = FileLock::acquire(&store).unwrap();
        guard.release();
        let _guard2 = FileLock::acquire(&store).unwrap();
    }

    #[test]
    fn reject_if_held_ok_when_unlocked() {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        FileLock::reject_if_held_by_other(&store).expect("unlocked store must allow Focus writes");
    }

    #[test]
    fn reject_if_held_same_process_ok() {
        // Same PID may hold CLI lock; Focus in same process is not "other".
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let store = Arc::new(store);
        let _guard = FileLock::acquire(&store).unwrap();
        FileLock::reject_if_held_by_other(store.as_ref())
            .expect("same-process lock holder is not 'other'");
    }
}
