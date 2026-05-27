# atlas-mcp

MCP (Model Context Protocol) server for Atlas. Exposes 24 tools over stdio JSON-RPC for AI coding assistants.

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
| `search` | `search.rs` | No — scoped store query, optional bounded structural parsing |
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
| `trace_forward` | `trace.rs` | No — uses RawTraceEngine directly |
| `language_capabilities` | `capability.rs` | No |
| `usages` | `usages.rs` | No — store queries |
| `dependencies` | `dependencies.rs` | No — store queries |
| `dependents` | `dependents.rs` | No — store queries |
| `task_status` | `mod.rs` task helpers | No — in-process task registry |
| `wait_for_task` | `wait_for.rs` | No — in-process task registry |

## Tool schema reference

The source of truth for MCP input schemas is `make_all_tools()` in
`crates/atlas-mcp/src/tools/mod.rs`. The current public tool names are short
names without the old `atlas_` prefix.

| Tool | Required arguments | Optional arguments |
|------|--------------------|--------------------|
| `index` | — | `include`: string[], `exclude`: string[], `background`: boolean |
| `open_project` | `project_path`: absolute path | `storage`: `"memory"` \| `"persistent"` (default `"memory"`), `scan_files`: boolean (default `false`), `background`: boolean |
| `status` | — | — |
| `files` | — | — |
| `search` | `query`: string, `scope`: string | `kind`: string, `limit`: integer (default 20), `background`: boolean |
| `symbol` | `qualified_name`: string | — |
| `neighbors` | `symbol`: qualified name | `direction`: `"outgoing"` \| `"incoming"` \| `"both"` (default `"both"`), `depth`: integer (default 1, max 3), `limit`: integer (default 50) |
| `callers` | `symbol`: qualified name | `limit`: integer (default 20) |
| `callees` | `symbol`: qualified name | `limit`: integer (default 20) |
| `callgraph` | `symbol`: qualified name | `depth`: integer (default 3, max 5), `limit`: integer (default 100) |
| `path` | `from`: qualified name, `to`: qualified name | `max_depth`: integer (default 5, max 10) |
| `explore` | `symbol`: qualified name | — |
| `impact` | `symbol`: qualified name | `depth`: integer (default 3, max 5) |
| `context` | `symbol`: qualified name | — |
| `trace_point` | `line`: integer, `column`: integer | `file_id`: hex string, `file_path`: project-relative path |
| `trace_variable` | `line`: integer, `column`: integer | `file_id`: hex string, `file_path`: project-relative path, `max_depth`: integer (default 30) |
| `trace_caller_path` | `symbol`: hex symbol id, `symbol_name`: lookup name | `max_depth`: integer (default 20) |
| `trace_forward` | `from`: hex symbol id, `to`: hex symbol id | `max_depth`: integer (default 10) |
| `language_capabilities` | — | — |
| `usages` | `symbol`: qualified name | `limit`: integer (default 50) |
| `dependencies` | `file_id`: hex string | `limit`: integer (default 50) |
| `dependents` | `file_id`: hex string | `limit`: integer (default 50) |
| `task_status` | `task_id`: string | — |
| `wait_for_task` | `task_id`: string | `timeout_secs`: integer (default 30, max 300), `poll_interval_secs`: integer (default 2, range 1–10) |

Notes:

- `trace_point` and `trace_variable` require a location (`line`, `column`) and
  accept either `file_id` or `file_path`; handlers return a structured error if
  neither file identity can be resolved.
- `trace_caller_path` accepts either `symbol` or `symbol_name`; handlers return a
  structured error when neither target is provided.
- `background: true` is currently supported by `search`, `index`, and
  `open_project`; use `task_status` or `wait_for_task` with the returned
  `task_id`.
- Clients that do not send MCP progress tokens are protected from long blocking
  calls: `index` is auto-started as a background task, and
  `open_project(scan_files=true)` is also auto-backgrounded. The initial tool
  result includes `task_id`, `progress: 0.0`, and `auto_background: true`; poll
  `task_status` to receive progress percentages.
- Clients that do send MCP progress tokens can run foreground `index` and
  receive `notifications/progress`; `search` also forwards progress
  notifications when a progress token is present.
- `open_project` only activates a project. It never indexes and does not accept
  `index`, `analysis`, `include`, or `exclude`. After activation, call `index`
  to index the active project.
- MCP `index` intentionally performs manifest indexing only: file records plus
  basic symbols/functions. It skips reference resolution and graph building so
  first index stays responsive on large repositories.
- `search` requires a project-relative `scope`. Without scope it returns an
  error and does not perform extraction or follow-up parsing. If the scope has
  at most 120 indexed files, search ensures structural data before querying; if
  the scope is larger, it returns manifest-level results and tells the client to
  narrow the scope for precise parsing.
- Large scoped searches schedule a small background structural preparse around
  returned files to improve follow-up latency without blocking the MCP request.
- `open_project` does not walk the project tree by default. Use `scan_files=true`
  only when you need an approximate `file_count` before indexing.
- `open_project(background=true)` prepares the project off the request path;
  the completed project is activated when `task_status` or `wait_for_task`
  observes completion.

## Key design decisions

- **Graph is lazily initialized**: `ToolRouter::ensure_graph_initialized()` is called by the MCP server layer before dispatching to graph-backed tools. Store-backed tools (search, trace, status, files, usages) skip graph construction entirely.
- **Background tasks are the compatibility progress channel**: progress-aware MCP clients use protocol progress notifications; clients without that support use the background task API and poll `task_status`.
- **Scope is mandatory for search**: `search` never performs global extraction. Scope size controls parsing depth: small scopes get bounded structural parsing; large scopes stay manifest-level with a narrowing warning.
- **Active project switching**: `open_project` can switch the active project at runtime. `activate_project()` atomically replaces the store, lazy service, and clears graph caches.
- **Memory storage mode**: `open_project(storage="memory")` opens an in-memory SQLite store for zero-footprint temporary sessions.
- **FileLock for persistent stores**: `index` acquires a cross-process exclusive lock before writing. `open_project(storage="persistent")` only opens and initializes the project database.
