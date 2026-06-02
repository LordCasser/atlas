# Atlas tool guide

## Installation and MCP configuration

Build with all languages and MCP:

```bash
cargo build --release -p atlas-cli --features "all-languages,mcp"
```

> `all-languages` includes TypeScript, JavaScript, Python, Java, C, C++, Go, C#, Rust, PHP, Ruby, Kotlin, ArkTS, and Cangjie.

Initialize and index a project before starting MCP:

```bash
atlas init --project /path/to/project
atlas index --project /path/to/project
atlas mcp --project /path/to/project
```

MCP client JSON:

```json
{
  "mcpServers": {
    "atlas": {
      "command": "/absolute/path/to/atlas",
      "args": ["mcp", "--project", "/absolute/path/to/project"]
    }
  }
}
```

Codex config:

```toml
[mcp_servers.atlas]
command = "/absolute/path/to/atlas"
args = ["mcp", "--project", "/absolute/path/to/project"]
enabled = true
```

## MCP tools

All 18 tools use short names (no `atlas_` prefix):

| Task | Tool | Key arguments |
| --- | --- | --- |
| Project overview | `project(action="status")` | none |
| Indexed files | `project(action="files")` | optional `limit`, `language`, `path_prefix` |
| Symbol search | `search` | `query` (required), optional `scope`, `kind`, `limit`, `background` |
| Symbol details | `symbol(view="detail")` | `qname` (required), optional `includeCode` |
| Agent context | `symbol(view="context")` | `qname` (required), optional `includeCode` |
| Symbol usages | `symbol(view="usages")` | `qname` (required), optional `limit` |
| Call graph | `calls` | `symbol` (required), `direction="incoming\|outgoing\|both"`, optional `depth`, `limit`, `edge_kinds` |
| Symbol exploration | `explore` | `symbol` (required), optional `includeCode` |
| Shortest path | `path` | `from`, `to` (required), optional `max_depth`, `direction`, `prefer_production`, `edge_kinds`, `includeCode` |
| Impact analysis | `impact` | `symbol` (required), optional `depth`, `semantic` |
| File dependencies | `file_dependencies` | `file_path` (required), `direction="incoming\|outgoing\|both"`, optional `limit` |
| Point inspection | `trace(kind="point")` | `file_path` or `file_id`, `line`, `column` |
| Variable origin | `trace(kind="variable")` | `file_path` or `file_id`, `line`, `column`, optional `max_depth` |
| Caller chain | `trace(kind="callers")` | `symbol` (qualified name or hex ID), optional `max_depth` |
| Forward call trace | `trace(kind="forward")` | `from`, `to` (qualified name or hex ID), optional `max_depth` |
| Index project | `index` | optional `include`, `exclude`, `background` |
| Open project | `project(action="open")` | `project_path` (required), optional `storage`, `scan_files`, `background` |
| FP dispatch annotations | `fp_dispatches` | `action="add\|list\|delete"` |
| Domain rules | `domain_rules` | `action="add\|list\|delete\|learn"` |
| Background tasks | `tasks` | optional `query_id` |
| Task status | `task_status` | `task_id` (required) |
| Wait for task | `wait_for_task` | `task_id` (required), optional `timeout_secs`, `poll_interval_secs` |
| Resume query | `resume_task` | `query_id` (required) |

## Query tactics

- Start with `search` for names. Use `kind:function`, `kind:class`, or shorter search terms if exact names fail.
- Convert user-visible file paths to IDs with `files` only when a tool requires `file_id`.
- Use shallow graph depths first (`depth: 1` or `2`) to avoid noisy results.
- For code-review or refactor questions, combine `impact` with `symbol(view="usages")` and `symbol(view="context")`.
- For debugging value flow, call `trace(kind="point")` first, then `trace(kind="variable")` at the same position.

## Trace response handling

Trace tools return an envelope with:

- `ok`: whether the query completed successfully.
- `kind`: trace result kind.
- `capability`: language capability metadata.
- `partial_result`: whether Atlas had to return incomplete evidence.
- `diagnostics`: warnings, unsupported features, or lookup ambiguity.
- `result`: the trace-specific payload.

When `partial_result` is true or diagnostics are non-empty, summarize the evidence and clearly state the limitation.

## Troubleshooting

- **No database**: run `atlas init` and `atlas index` for the project.
- **Stale result**: run `atlas sync`; restart MCP only if the client does not reconnect or refresh its server process.
- **No symbol found**: retry `search` with shorter names, no kind filter, or larger `limit`.
- **Trace is empty**: check `language_capabilities`; the language or construct may only support symbolic graph queries.
- **Huge output**: reduce `limit`, lower graph `depth`, or query a more specific symbol.
