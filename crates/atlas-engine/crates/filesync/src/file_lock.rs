//! File lock: prevents concurrent atlas processes from writing the same database.
//!
//! Uses SQLite's own `BEGIN EXCLUSIVE` transaction as the locking mechanism.
//! No OS-level flock or external dependency — SQLite already guarantees
//! cross-process mutual exclusion for writes.
//!
//! ## Usage
//!
//! ```ignore
//! let guard = FileLock::acquire(&store)?;
//! // ... exclusive write access to the database ...
//! drop(guard); // transaction rolled back, lock released
//! ```

use db::Store;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// FileLockGuard
// ---------------------------------------------------------------------------

/// RAII guard that holds an exclusive SQLite transaction.
/// The lock is released (transaction rolled back) when this guard is dropped.
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
        // Roll back the exclusive transaction to release the lock.
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

/// Acquire an exclusive write lock on the Atlas database via SQLite.
///
/// Internally this executes `BEGIN EXCLUSIVE`, which:
/// - Blocks until no other connection is reading or writing
/// - Prevents all other connections from reading or writing until committed/rolled back
///
/// This is the correct way to prevent concurrent atlas processes — SQLite
/// already handles cross-process locking internally.
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
