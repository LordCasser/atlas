# atlas-mcp

MCP (Model Context Protocol) server for Atlas. Exposes 18 tools over stdio JSON-RPC for AI coding assistants.

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
| `project` | `open_project.rs` | No — switches active project |
| `index` | `index.rs` | No — writes to store |
| `tasks` | `mod.rs` task helpers | No — lists in-process tasks |
| `task_status` | `mod.rs` task helpers | No — task registry |
| `wait_for_task` | `wait_for.rs` | No — task completion polling |
| `resume_task` | `resume.rs` | No — re-executes snapshotted query |
| `search` | `search.rs` | No — scoped store query, optional bounded structural parsing |
| `symbol` | `search.rs` | Yes |
| `calls` | `mod.rs` | Partial — call graph required for multi-hop |
| `path` | `graph.rs` | Yes |
| `explore` | `graph.rs` | Yes |
| `impact` | `graph.rs` | Yes |
| `file_dependencies` | `mod.rs` | No — store queries |
| `trace` | `trace.rs` | No — uses RawTraceEngine directly; lazy-loads dataflow internally |
| `lifecycle` | `lifecycle.rs` | No — consumes CFG+DataFlow from store |
| `branch_diff` | `branch_diff.rs` | No — consumes CFG+DataFlow from store |
| `fp_dispatches` | `mod.rs` | No — store queries/writes |
| `domain_rules` | `mod.rs` | No — store queries/writes |

## Tool schema reference

The source of truth for MCP input schemas is `make_all_tools()` in
`crates/atlas-mcp/src/tools/mod.rs`. The current public tool names are short
names without the old `atlas_` prefix.

| Tool | Required arguments | Optional arguments |
|------|--------------------|--------------------|
| `project` | `action`: `"open"`\|`"status"`\|`"files"` | `project_path`, `storage`, `scan_files`, `background` |
| `index` | — | `include`: string[], `exclude`: string[], `background`: boolean |
| `search` | `query`: string | `scope`: string, `kind`: string, `limit`: integer (default 20), `background`: boolean |
| `symbol` | `symbol`: qualified name | `view`: `"detail"`\|`"context"`\|`"usages"`, `includeCode`: boolean |
| `calls` | `symbol`: qualified name | `direction`: `"incoming"`\|`"outgoing"`\|`"both"`, `depth`: integer (default 1, max 5), `edge_kinds`: string[] (default `["calls","instantiates","implements"]`, `[]` or `["*"]` for all) |
| `path` | `from`: qualified name, `to`: qualified name | `max_depth`: integer (default 5, max 10), `direction`, `edge_kinds`, `includeCode` |
| `explore` | `symbol`: qualified name | `includeCode`: boolean (default `false`) |
| `impact` | `symbol`: qualified name | `depth`: integer (default 3, max 5) |
| `file_dependencies` | `file_path`: project-relative or absolute path | `direction`: `"incoming"`\|`"outgoing"`\|`"both"`, `limit`: integer |
| `trace` | `kind`: `"point"`\|`"variable"`\|`"forward"`\|`"callers"` | `symbol`/`from`/`to`/`line`/`column`/`file_id` (depends on kind) |
| `lifecycle` | `symbol`: qualified name, `field`: string | `include_proof`: boolean |
| `branch_diff` | `symbol`: qualified name | `semantic`: boolean (default `true`) |
| `fp_dispatches` | `action`: `"add"`\|`"list"`\|`"delete"` | `field_qname`, `target_qname`, `annotation_id` |
| `domain_rules` | `action`: `"add"`\|`"list"`\|`"delete"`\|`"learn"` | `language`, `kind`, `patterns`, `status` |
| `tasks` | — | — |
| `task_status` | `task_id`: string | — |
| `wait_for_task` | `task_id`: string | `timeout_secs`: integer (default 30, max 300), `poll_interval_secs`: integer (default 2) |
| `resume_task` | `query_id`: string | — |

Notes:

- `trace` accepts a `kind` parameter: `"point"` (single location), `"variable"` (dataflow trace), `"forward"` (path between two symbols), or `"callers"` (caller chain). Each kind has different required args (`symbol`/`from`/`to`/`line`/`column`/`file_id`).
- `calls` with `direction="incoming"` or `"outgoing"` and `depth=1` replaces old `callers`/`callees`. Multi-hop uses `direction="both"` and `depth>1`.
- `fp_dispatches` with `action: "add"|"list"|"delete"` replaces old `annotate_fp_dispatch`, `list_fp_annotations`, `delete_fp_annotation`.
- `project` with `action: "open"|"status"|"files"` replaces old `open_project`, `status`, `files`.
- `lifecycle` and `branch_diff` are new analysis tools consuming CFG+DataFlow.
- `background: true` is currently supported by `search`, `index`, and
  `project`; use `task_status` or `wait_for_task` with the returned
  `task_id`.
- Clients that do not send MCP progress tokens are protected from long blocking
  calls: `index` is auto-started as a background task, and
  `project(scan_files=true)` is also auto-backgrounded.
- `project` only activates a project. It never indexes. After activation, call `index`.
- MCP `index` intentionally performs manifest indexing only.
- `search` requires a `query` string; `scope` is required for manifest-only indexes.
- `project` does not walk the project tree by default. Use `scan_files=true`
  only when you need an approximate `file_count`.

### Request-scoped include roots

For C/C++ projects, you can pass `include_roots` to help resolve `#include <...>` during lazy structural extraction. The roots are project-relative, request-scoped, and not persisted. Default auto-detection includes `project_root/include/`.

Example:
```json
{"query": "do_sched", "scope": "kernel/sched", "include_roots": ["include", "third_party/include"]}
```

## Key design decisions

- **Graph is lazily initialized**: `ToolRouter::ensure_graph_initialized()` is called by the MCP server layer before dispatching to graph-backed tools. Store-backed tools (`search`, `trace`, `project`, `file_dependencies`, `symbol(view="usages")`) skip graph construction entirely.
- **Background tasks are the compatibility progress channel**: progress-aware MCP clients use protocol progress notifications; clients without that support use the background task API and poll `task_status`.
- **Scope is mandatory for search**: `search` never performs global extraction. Scope size controls parsing depth: small scopes get bounded structural parsing; large scopes stay manifest-level with a narrowing warning.
- **Active project switching**: `project(action="open")` can switch the active project at runtime. `activate_project()` atomically replaces the store, lazy service, and clears graph caches.
- **Memory storage mode**: `project(action="open", storage="memory")` opens an in-memory SQLite store for zero-footprint temporary sessions.
- **FileLock for persistent stores**: `index` acquires a cross-process exclusive lock before writing. `project(action="open", storage="persistent")` only opens and initializes the project database.
