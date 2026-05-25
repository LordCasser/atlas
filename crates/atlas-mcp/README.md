# atlas-mcp

MCP (Model Context Protocol) server for Atlas. Exposes 20 tools over stdio JSON-RPC for AI coding assistants.

## Architecture

```
rmcp::transport::stdio()
    │
    ▼
AtlasMcpService (ServerHandler)
    ├── list_tools → make_all_tools()
    └── call_tool
        ├── ensure_graph_initialized (lazy, on first graph-backed call)
        ├── maybe_refresh_graph (detect external index changes)
        └── ToolRouter::call_tool() → dispatch to handlers
```

## Tools

| Tool | Handler module | Requires graph? |
|------|---------------|-----------------|
| `open_project` | `open_project.rs` | No — switches active project |
| `index` | `index.rs` | No — writes to store |
| `status` | `status.rs` | No — store queries |
| `files` | `status.rs` | No — store queries |
| `search` | `search.rs` | Yes |
| `symbol` | `search.rs` | Yes |
| `neighbors` | `graph.rs` | Yes |
| `callers` | `graph.rs` | Yes |
| `callees` | `graph.rs` | Yes |
| `callgraph` | `graph.rs` | Yes |
| `path` | `graph.rs` | Yes |
| `explore` | `graph.rs` | Yes |
| `impact` | `graph.rs` | Yes |
| `context` | `context.rs` | Yes |
| `trace_point` | `trace.rs` | No — uses RawTraceEngine directly |
| `trace_variable` | `trace.rs` | No — lazy-loads dataflow internally |
| `trace_caller_path` | `trace.rs` | No — uses RawTraceEngine directly |
| `language_capabilities` | `capability.rs` | No |
| `usages` | `usages.rs` | No — store queries |
| `dependencies` | `dependencies.rs` | No — store queries |
| `dependents` | `dependents.rs` | No — store queries |

## Key design decisions

- **Graph is lazily initialized**: `ToolRouter::ensure_graph_initialized()` is called by the MCP server layer before dispatching to graph-backed tools. Store-backed tools (trace, status, files, usages) skip graph construction entirely.
- **Active project switching**: `open_project` can switch the active project at runtime. `activate_project()` atomically replaces the store, lazy service, and clears graph caches.
- **Memory storage mode**: `open_project(storage="memory")` opens an in-memory SQLite store for zero-footprint temporary sessions.
- **FileLock for persistent stores**: Both `open_project(storage="persistent")` and `index` acquire a cross-process exclusive lock before writing.
