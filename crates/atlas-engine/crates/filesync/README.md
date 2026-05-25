# filesync

File discovery, content hashing, incremental sync, and cross-process file locking.

## Components

### discovery

Git-aware file discovery with include/exclude glob filters.
Falls back to filesystem traversal when no Git repository is found.

### detector

Content-hash based dirty file detection. Compares stored `content_hash` in the database with `blake3(file_contents)` on disk.

### watcher

(Planned) Filesystem watcher integration for automatic re-indexing.

### FileLock

Cross-process exclusive write lock for `.atlas/atlas.db`.

```rust
let guard = FileLock::acquire(&store)?;
// ... exclusive write access ...
drop(guard); // lock released
```

Uses SQLite's `BEGIN IMMEDIATE` + `project_metadata` table. Locks are process-scoped (PID-based) with stale-lock stealing (dead process → lock released).

## Public API

```rust
// Discovery
DiscoveryConfig { include_patterns, exclude_patterns, ... }
discover_files(root, config) → Vec<PathBuf>

// Sync
SyncEngine::new(store) → SyncEngine
SyncEngine::sync(root) → SyncStats

// Locking
FileLock::acquire(&store) → FileLockGuard
```
