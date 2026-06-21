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

// In-memory (tests only; production MCP uses the persistent project DB)
let store = Store::open_in_memory()?;
store.init_schema()?;

// Access the database path
let path = store.db_path();  // ":memory:" or filesystem path
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
| `project_metadata` | `key TEXT` | Project-level settings, exclusive lock state |
| `extraction_state` | `(file_id, unit_id, layer)` | Unified file/unit extraction state |
| `extraction_jobs` | `job_id TEXT` | Active/completed extraction jobs |
| `domain_rules` | `id TEXT` | Language-neutral domain rules and candidate status |
| `function_pointer_annotations` | `annotation_id TEXT` | User-declared function-pointer dispatch mappings |
| `function_summaries` | `function_id BLOB(32)` | Per-function intra-procedural summary |
| `summary_param_reaches` | logical composite row | Which params reach which locals |
| `summary_return_sources` | logical composite row | Which locals flow to return |
| `summary_call_arg_sources` | logical composite row | Which locals flow to call args |
| `closure_generations` | `closure_id TEXT` | Focus closure generation and commit state |
| `closure_coverage` | composite key | Per-closure file coverage materialization |
| `reference_resolutions` | composite key | Closure-scoped resolution candidates/results |
| `symbol_edge_candidates` | composite key | Closure-scoped graph-edge candidates |
| `file_inventory` | `file_id BLOB(32)` | Focus bootstrap file inventory and fingerprints |
| `symbol_hints` | composite key | Lightweight symbol hints for cold scoped queries |

Schema V2 contains 28 entity tables plus the `symbols_fts` virtual table. Focus control-plane rows are transient session materialization; canonical source facts and symbol edges remain durable in the same SQLite database.
