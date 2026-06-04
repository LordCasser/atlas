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

impl FileLock {
    /// Acquire an exclusive lock. Blocks until available.
    pub fn acquire(store: &Arc<Store>) -> anyhow::Result<FileLockGuard> {
        store.acquire_exclusive_lock()?;
        Ok(FileLockGuard {
            store: Arc::clone(store),
        })
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
}
