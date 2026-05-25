# atlas-engine

Public facade crate for Atlas. Re-exports all core types and provides the high-level `Engine` struct.

## Re-exports

| Module | Key exports |
|--------|-------------|
| `types` | All IR types (`SymbolDef`, `FileFacts`, `ReferenceUse`, ...) |
| `db` | `Store`, `CURRENT_SCHEMA_VERSION` |
| `workspace` | `Workspace`, `ProjectRoot`, `SourcePath` |
| `extraction` | `ExtractionMode`, `LanguageFrontend`, `ParseWorkerPool`, `create_frontend` |
| `resolution` | `ReferenceResolver`, `PathAliasResolver` |
| `graph` | `GraphEngine`, `GraphBuilder`, `GraphSnapshot`, `TraversalConfig` |
| `lazy` | `LazyDataflowService` (on-demand dataflow loading) |
| `analysis::trace` | `TraceEngine` (as `RawTraceEngine`), `TraceQueryResponse` |
| `search` | `SearchEngine` |
| `context` | `ContextBuilder` |
| `filesync` | `FileLock`, `SyncEngine`, `discovery` |

## Engine struct

The `Engine` struct is a convenience wrapper for testing and simple use cases. Production code (MCP server, CLI commands) typically constructs `Store`, `LazyDataflowService`, and trace engines directly.

For detailed architecture, see [`docs/architecture.md`](../../docs/architecture.md).
