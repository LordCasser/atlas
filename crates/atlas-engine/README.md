# atlas-engine

Public facade crate for Atlas. Re-exports all core types and provides the high-level `Engine` struct.

## Re-exports

| Module | Key exports |
|--------|-------------|
| `types` | All IR types (`SymbolDef`, `FileFacts`, `ReferenceUse`, ...) |
| `db` | `Store`, `CURRENT_SCHEMA_VERSION` |
| `workspace` | `Workspace`, `ProjectRoot`, `SourcePath` |
| `extraction` | `ExtractionMode`, `LanguageFrontend`, `ParseWorkerPool`, `create_frontend` |
| `resolution` | `ReferenceResolver`, `PathAliasResolver`, `PATH_ALIAS_CONFIG_FILES` |
| `graph` | `GraphEngine`, `GraphBuilder`, `GraphSnapshot`, `TraversalConfig` |
| `lazy` | `LazyDataflowService` (on-demand dataflow loading) |
| `analysis::trace` | `TraceEngine` (as `RawTraceEngine`), `TraceQueryResponse` |
| `search` | `SearchEngine` |
| `context` | `ContextBuilder` |
| `dossier` | Symbol dossier request/response types and builder |
| `filesync` | `FileLock`, `SyncEngine`, `discovery`, `build_dirty_set`, `clean_stale_file_ids`, `run_index_pipeline` |

## Engine struct

The high-level `Engine` owns the user-facing trace path and triggers lazy dataflow before delegating to raw analysis consumers. Raw `analysis::TraceEngine` only reads facts already present in the store. CLI and MCP should use facade/services and shared filesync/focus orchestration rather than rebuilding extraction, resolution, graph, or trace pipelines in entry-point code.

For detailed architecture, see [`../../docs/architecture.md`](../../docs/architecture.md).
