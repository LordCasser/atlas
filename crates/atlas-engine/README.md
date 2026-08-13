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
| `focus` materialize | `FocusMaterialize`, `LazyDataflowService`, `LazyStructuralService` (Focus-internal ensure stack; not a product path) |
| `analysis::trace` | `TraceEngine` (as `RawTraceEngine`), `TraceQueryResponse` |
| `search` | `SearchEngine` |
| `context` | `ContextBuilder` |
| `dossier` | Symbol dossier request/response types and builder |
| `filesync` | `FileLock`, `SyncEngine`, `discovery`, `build_dirty_set`, `clean_stale_file_ids`, `run_index_pipeline` |

## Engine struct

The high-level `Engine` owns the user-facing trace path and triggers **Focus materialize** (on-demand dataflow) before delegating to raw analysis consumers. Prefer `Engine::from_materialize` when sharing one stack with FocusRuntime/MCP; `from_store` opens a new stack (CLI/TUI process boundary).

Raw `analysis::TraceEngine` only reads facts already present in the store. CLI and MCP should use facade/services and shared filesync/Focus orchestration rather than rebuilding extraction, resolution, graph, or trace pipelines in entry-point code.

`Engine::extract_file_with_mode` accepts a project-relative `SourcePath` and derives
Atlas' path-based `FileId`. Blob/version consumers must call the lower-level
`extraction::extract_file_with_mode` with a caller-owned identity; this keeps the
future Corpus blob model out of the single-workspace facade.

For detailed architecture, see [`../../docs/architecture.md`](../../docs/architecture.md) §2.1.1 / §7.1 / §10.1.11.
