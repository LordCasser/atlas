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
///
/// Type alias for the shared `db::ExclusiveLockHeld` diagnostic (single source).
pub type IndexLockHeld = db::ExclusiveLockHeld;

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
    /// Delegates to [`Store::reject_if_exclusive_lock_held_by_other`] (shared diagnostics).
    pub fn reject_if_held_by_other(store: &Store) -> Result<(), IndexLockHeld> {
        store.reject_if_exclusive_lock_held_by_other()
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

    #[test]
    fn reject_if_held_by_foreign_live_pid() {
        // Simulate another process holding the lock with a high PID that is
        // very unlikely to be alive on this machine; if the platform reports
        // it alive, skip (flaky environments).
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let foreign_pid: i64 = 2_147_000_000;
        store
            .set_metadata(
                "exclusive_lock_pid",
                &format!("{foreign_pid}:0"),
            )
            .unwrap();
        match store.exclusive_lock_held_by_other().unwrap() {
            Some(pid) => {
                assert_eq!(pid as i64, foreign_pid);
                let err = FileLock::reject_if_held_by_other(&store).expect_err("must reject");
                assert_eq!(err.code, "cli_index_lock_held");
                assert!(!err.reason.is_empty());
                assert!(!err.suggested_action.is_empty());
                assert!(err.to_string().contains("cli_index_lock_held"));
            }
            None => {
                // Kernel reports that PID as not alive — lock treated as stale.
                FileLock::reject_if_held_by_other(&store).expect("stale lock not held");
            }
        }
    }
}
