# Atlas DB — SQLite Persistence Layer

SQLite-backed persistence layer for Atlas semantic graph data.

Exported from this crate:
- **`Store`** — thread-safe SQLite persistence with CRUD + FTS5
- **`StoreStats`** — aggregate statistics
- **`CURRENT_SCHEMA_VERSION`** / **`SCHEMA_DDL`** — schema constants

## Lifecycle

```rust
// Open a database at an explicit file path (no directory creation)
let store = Store::open_db(Path::new("/path/to/.atlas/atlas.db"))?;

// Initialize the schema (idempotent)
store.init_schema()?;

// In-memory (for tests)
let store = Store::open_in_memory()?;
store.init_schema()?;
```

Callers are responsible for creating `.atlas/` and any parent directories
(via `Workspace::ensure_atlas_dir`) before calling `Store::open_db`.

## Schema Tables

| Table | Primary Key | Description |
|-------|-------------|-------------|
| `files` | `file_id BLOB(32)` | Indexed source files |
| `symbols` | `symbol_id BLOB(32)` | All symbol definitions |
| `scopes` | `scope_id BLOB(32)` | Lexical scopes |
| `references` | `ref_id BLOB(32)` | Symbol references |
| `imports` | `import_id BLOB(32)` | Import declarations |
| `symbol_edges` | `edge_id BLOB(32)` | Symbol-level relationships |
| `callsites` | `callsite_id BLOB(32)` | Function call sites |
| `bindings` | `binding_id BLOB(32)` | Lexical binding definitions |
| `binding_uses` | `binding_use_id BLOB(32)` | Lexical binding references |
| `data_nodes` | `data_node_id BLOB(32)` | Local dataflow nodes |
| `dataflow_edges` | `dataflow_edge_id BLOB(32)` | DataNode provenance edges |
| `cfg_nodes` | `cfg_node_id BLOB(32)` | Function-local CFG nodes |
| `cfg_edges` | `cfg_edge_id BLOB(32)` | Function-local CFG edges |
| `symbols_fts` | (FTS5) | Full-text search over symbol names |
| `project_metadata` | `key TEXT` | Project-level settings |
| `schema_versions` | `version INTEGER` | Schema history marker |
