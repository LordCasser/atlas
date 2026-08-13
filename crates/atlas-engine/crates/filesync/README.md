# filesync

File discovery, content hashing, incremental sync, and cross-process file locking.

## Components

### discovery

Git-aware file discovery with include/exclude glob filters.
Falls back to filesystem traversal when no Git repository is found.

### detector

Content-hash based dirty file detection. Compares stored `content_hash` in the database with `blake3(file_contents)` on disk.
Git status is not a change authority: it is relative to `HEAD`, not to the last
Atlas database state. Incremental sync therefore compares the canonical discovered
file set and raw content hashes directly with SQLite; Git is used only by discovery.

### dirty

Shared hash-based dirty-set computation for full indexing. It compares
project-relative discovered files against stored DB hashes and returns dirty,
clean, and deleted file sets without performing extraction or cleanup.

### index_pipeline

Entry-point-neutral indexing pipeline. It discovers files, cleans stale facts,
extracts `FileFacts`, and optionally runs reference resolution plus graph edge
building for non-manifest modes. CLI, MCP, and sync callers remain responsible
for locking, UI/progress, background execution, and choosing the extraction mode.

### cleanup

Shared stale-index cleanup. It invalidates incoming references to old symbols,
deletes outgoing edges derived from old file references, removes old file facts,
and clears per-file index layer records before callers insert replacement facts.

### watcher

Optional `notify`-based recursive file events behind the crate's `sync` feature. The watcher emits created/modified/removed paths; callers decide when and how to invoke incremental synchronization.

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

// Dirty set
build_dirty_set(store, discovered, root) → DirtySet

// Shared index pipeline
run_index_pipeline(store, root, IndexPipelineOptions::new(mode)) → IndexPipelineStats

// Stateful full/incremental orchestration
IndexPipeline::new(store, root, options) → IndexPipeline
IncrementalPipeline::new(store, root, mode) → IncrementalPipeline

// Shared cleanup
clean_stale_file_paths(store, paths) → Vec<FileId>
clean_stale_file_ids(store, file_ids) → ()

// Locking
FileLock::acquire(&store) → FileLockGuard
```
