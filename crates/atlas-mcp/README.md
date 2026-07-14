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

| Tool | Actual function | Required facts / Focus behavior |
|------|-----------------|---------------------------------|
| `project` | Open, status, or known-file inventory. | No query Focus; `open` never scans the repository. |
| `search` | Name search inside mandatory `scope`. | Structural candidate region; manifest is only the cold-start candidate layer. |
| `symbol` | Detail, usages, or structured context. | Detail/source=structural; usages/context=call graph. |
| `calls` | Fixed one-hop incoming/outgoing or bounded multi-hop `both`. | Cross-file call-graph hot region in the requested direction/depth. |
| `explore` | Symbol dossier: source, call evidence, relations, file context. | Call graph plus import neighborhood. |
| `path` | Ranked graph paths between two resolved symbols. | Cross-file call-graph region grown toward both endpoints. |
| `impact` | Bounded affected-symbol/file traversal; optional semantic overlay. | Call graph; `semantic=true` upgrades to dataflow/CFG. |
| `file_dependencies` | Incoming/outgoing imports/includes for one file. | Manifest by default; `analysis=structural` upgrades the file neighborhood. |
| `trace` | Point lookup, variable provenance, forward chain, or caller chain. | Point=structural; forward/callers=call graph; variable=dataflow over a widened cross-file dependency region. |
| `lifecycle` | C/C++ field allocate/use/free state analysis. | Function CFG, dataflow effects, and domain rules via tracked Sync Focus work. |
| `branch_diff` | Compare sibling branch side effects/asymmetries. | Function CFG/dataflow effects via tracked Sync Focus work. |
| `fp_dispatches` | Manage user-supplied function-pointer targets. | Overlay read/write; no Focus parsing. |
| `domain_rules` | Manage or learn ownership/allocation rules. | Overlay read/write; no Focus parsing. |
| `tasks` | Observe current Focus/lazy activity. | Observation only; does not create another task model. |
| `resume_query` | Replay the original query snapshot. | Reuses the original tool's required fact level and tracked jobs. |

Removed MCP tools: `index`, `task_status`, `wait_for_task`, and
`resume_task`. Removed MCP parameters: `project.background`,
`project.scan_files`, `project.force_memory`, and `search.background`.

## Query Semantics

- Every Focus-backed query has one 18-second interactive deadline. If tracked
  work reaches the tool's required fact level before the deadline, MCP
  transparently replays the existing `QuerySnapshot` and returns the complete
  result.
- If the deadline expires, MCP returns only `status=in_progress`, `query_id`,
  `pending.reason`, `pending.required_analysis`, and
  `analysis.retry_after_ms`. It never publishes provisional result arrays,
  paths, callers, or trace payloads. Background work continues and the caller
  resumes with `resume_query`.
- If tracked materialization fails, MCP returns a result-free `status=failed`
  ticket with the original `query_id` and failure reason. Re-run the original
  tool call to retry; failed work is never exposed as a limited result.
- `QueryNeed` is shared by MCP contracts and the Focus control plane:
  `manifest`, `structural`, `call_graph`, or `dataflow`.

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

Pending response shape:

```json
{
  "status": "in_progress",
  "tool": "trace",
  "query_id": "q_...",
  "pending": {
    "reason": "focus_dataflow_not_ready",
    "required_analysis": "dataflow",
    "detail": "Focus analysis still expanding: 1 pending job(s) remaining."
  },
  "analysis": {
    "scope": "local",
    "summary": "Focus is still building the dataflow facts required for this query; no partial result is published.",
    "retry_after_ms": 5000
  }
}
```

## Request-Scoped Include Roots

For C/C++ projects, pass `include_roots` to help resolve `#include <...>` during
lazy structural extraction. The roots are project-relative, request-scoped, and
not persisted. Default auto-detection includes `project_root/include/`. Tools
that prepare focus closures accept the same parameter, including `search`,
`symbol`, `calls`, `explore`, `path`, `trace`, `lifecycle`, and `branch_diff`.

```json
{"query": "do_sched", "scope": "kernel/sched", "include_roots": ["include"]}
```

## Source Of Truth

The tool schemas live in `make_all_tools()` in
`crates/atlas-mcp/src/tools/mod.rs`. Regression tests in
`crates/atlas-mcp/tests/schema_validation.rs` assert that removed indexing and
background-task parameters do not reappear.
