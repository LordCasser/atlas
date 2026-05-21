# Atlas Store

SQLite-backed persistence layer for Atlas semantic graph data.

## Schema (`schema.rs`)

### Tables

| Table | Primary Key | Description |
|-------|-------------|-------------|
| `files` | `file_id BLOB(32)` | Indexed source files |
| `symbols` | `symbol_id BLOB(32)` | All symbol definitions |
| `scopes` | `scope_id BLOB(32)` | Lexical scopes |
| `references` | `ref_id BLOB(32)` | Symbol references (preserved after resolution) |
| `imports` | `import_id BLOB(32)` | Import declarations |
| `symbol_edges` | `edge_id BLOB(32)` | Symbol-level relationships with confidence |
| `callsites` | `callsite_id BLOB(32)` | Function call sites |
| `bindings` | `binding_id BLOB(32)` | Lexical binding definitions |
| `binding_uses` | `binding_use_id BLOB(32)` | Lexical binding references |
| `data_nodes` | `data_node_id BLOB(32)` | Local dataflow nodes |
| `dataflow_edges` | `dataflow_edge_id BLOB(32)` | DataNode-to-DataNode provenance edges |
| `callsite_args` | composite | Deprecated table; current call args are stored inline on callsites |
| `cfg_nodes` | `cfg_node_id BLOB(32)` | Function-local CFG nodes |
| `cfg_edges` | `cfg_edge_id BLOB(32)` | Function-local CFG edges |
| `symbols_fts` | (FTS5) | Full-text search over symbol names and qualified names |
| `project_metadata` | `key TEXT` | Project-level settings |
| `schema_versions` | `version INTEGER` | Schema history marker |

### Key Design Decisions

1. **BLOB primary keys** — all IDs are 32-byte blake3 hashes, stored as SQLite BLOBs
2. **FTS5 over symbols** — triggers auto-sync `symbols_fts` on inserts/updates/deletes
3. **Foreign keys** — `references.source_symbol → symbols.symbol_id`; `symbol_edges` link symbol pairs; dataflow edges link data nodes
4. **Cascade deletes** — deleting a file removes all its symbols, scopes, references, imports, callsites

### Indexes

- `symbols.file_id` — find all symbols in a file
- `references.file_id` — find all references in a file
- `references.source_symbol` — find all references to a symbol
- `symbol_edges.source` / `symbol_edges.target` — symbol adjacency traversal
- `dataflow_edges.source` / `dataflow_edges.target` — provenance traversal
- `imports.file_id` — find all imports in a file

## Store API (`store.rs`)

```rust
pub struct Store {
    conn: Mutex<Connection>,
    db_path: PathBuf,
}
```

### Lifecycle

```rust
Store::open(db_path) -> Result<Self>
Store::init_project(root) -> Result<Self>  // finds/create .atlas/ dir, creates DB
```

### CRUD Operations

| Method | Description |
|--------|-------------|
| `insert_file(file_info)` | Upsert file metadata |
| `delete_file(file_id)` | Cascade-delete all data for a file |
| `insert_symbols(symbols)` | Batch-insert symbols |
| `insert_scopes(scopes)` | Batch-insert scopes |
| `insert_references(refs)` | Batch-insert references |
| `insert_imports(imports)` | Batch-insert imports |
| `insert_edges(edges)` | Batch-insert symbol edges |
| `insert_callsites(callsites)` | Batch-insert callsites |
| `insert_file_facts(facts)` | Transactionally write all facts for one extracted file |
| `get_symbols_by_file(file_id)` | Query symbols in a file |
| `get_references_by_file(file_id)` | Query references in a file |
| `get_references_to_symbol(sid)` | Incoming references |
| `find_edges_by_source(source)` | Outgoing symbol edges |
| `find_edges_by_target(target)` | Incoming symbol edges |
| `find_dataflow_edges_by_source(source)` | Outgoing dataflow edges |
| `find_dataflow_edges_by_target(target)` | Incoming dataflow edges |
| `get_stats()` | Returns `StoreStats` |
| `resolve_reference(ref_id, target)` | Mark a reference as resolved |

### StoreStats

```rust
struct StoreStats {
    total_files: i64,
    total_symbols: i64,
    total_references: i64,
    unresolved_references: i64,
    total_edges: i64,
    total_imports: i64,
    total_callsites: i64,
}
```

## Architecture Notes

- **Why Mutex<Connection>?** — Atlas is local-first. SQLite's WAL mode handles concurrent reads well. A single writer lock is sufficient for MVP.
- **Rapid schema iteration** — `CURRENT_SCHEMA_VERSION` is v1 during rapid development. Update schema docs and tests with schema changes; deployment migrations are not a current requirement.
- **Future: StoreWriter/StoreReader** — For performance, the Store may split into a writer (with lock) and a connection-pooled reader.
- **Batch inserts use transactions** — all `insert_*` methods wrap in `conn.execute("BEGIN")` / `conn.execute("COMMIT")` for atomicity.

## Test Helpers

- `test_db()` — creates a temporary in-memory SQLite store
- `sample_file_id()` / `sample_symbol()` / `sample_reference()` — factory functions for test data
