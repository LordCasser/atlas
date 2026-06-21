# atlas-mcp

MCP (Model Context Protocol) server for Atlas. The server starts without an
active project; clients must call `project(action="open")` before code-analysis
tools are available.

## Runtime Flow

```
rmcp stdio JSON-RPC
    |
    v
AtlasMcpService
    |-- list_tools -> make_all_tools()
    `-- call_tool
        |-- project(status/open/files) may run before an active project
        |-- active project required for all code queries
        |-- scoped query prepares FocusRuntime when no rich index exists
        |-- graph cache refreshes after focus/lazy writes
        `-- ToolRouter handler
```

`project(action="open")` is synchronous and does not scan or index the whole
tree. It opens or creates `project/.atlas/atlas.db`; focus-produced facts are
written there and reused by later MCP sessions. SQLite provides the transparent
page cache, mmap, and WAL-backed persistence layer, while Atlas focus controls
which local closures are analyzed on demand.

Explicit project-wide indexing is CLI-only: run `atlas index` outside MCP when
you want a reusable full-project cache.

## Tools

| Tool | Purpose |
|------|---------|
| `project` | Open a project, inspect active status, or list known files. |
| `search` | Search symbols inside a required project-relative `scope`; the scope is also the focus seed. |
| `symbol` | Return symbol detail, context, or usages. |
| `calls` | Incoming/outgoing call exploration. Outgoing results also expose unresolved call tokens such as external helpers/macros. |
| `explore` | Symbol dossier with source, call evidence, and related context. |
| `path` | Find graph paths between resolved local symbols. |
| `impact` | Traverse impacted symbols/files from a resolved local symbol. |
| `file_dependencies` | Store-backed file dependency facts. |
| `trace` | `point`, `variable`, `forward`, and `callers` tracing. |
| `lifecycle` | CFG/dataflow lifecycle analysis. |
| `branch_diff` | CFG/dataflow comparison across branch-like variants. |
| `fp_dispatches` | Add/list/delete manual function-pointer dispatch annotations. |
| `domain_rules` | Add/list/delete domain rules or learn candidate rules. |
| `tasks` | Inspect current focus/lazy extraction activity. |
| `resume_query` | Rehydrate a recent query snapshot after lazy focus work has progressed. |

Removed MCP tools: `index`, `task_status`, `wait_for_task`, and
`resume_task`. Removed MCP parameters: `project.background`,
`project.scan_files`, `project.force_memory`, and `search.background`.

## Query Semantics

- `search.scope` is mandatory even when a rich index exists. It bounds the
  answer and seeds focus coverage. Non-terminal work is exposed through
  `analysis.retry_after_ms`; terminal boundary limitations use `gaps`.
- A scoped query on an empty store triggers focus-driven extraction for the
  relevant files. Larger scopes may return `analysis.retry_after_ms` and a
  `query_id`; call `resume_query` until the response is terminal. Terminal
  limitations appear as structured `gaps`.
- `path` and `trace(kind="forward")` require both endpoints to resolve to local
  symbols. For external/helper calls that appear only as unresolved call tokens,
  use `calls(direction="outgoing")` and inspect `unresolved_callees`, or use
  `trace(kind="point")` at the callsite.
- `fp_dispatches` and `domain_rules` remain MCP mutation tools because they
  model user-supplied analysis facts, not indexing.

## Request-Scoped Include Roots

For C/C++ projects, pass `include_roots` to help resolve `#include <...>` during
lazy structural extraction. The roots are project-relative, request-scoped, and
not persisted. Default auto-detection includes `project_root/include/`.

```json
{"query": "do_sched", "scope": "kernel/sched", "include_roots": ["include"]}
```

## Source Of Truth

The tool schemas live in `make_all_tools()` in
`crates/atlas-mcp/src/tools/mod.rs`. Regression tests in
`crates/atlas-mcp/tests/schema_validation.rs` assert that removed indexing and
background-task parameters do not reappear.
