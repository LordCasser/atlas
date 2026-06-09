# Atlas tool guide

## Installation and MCP configuration

Build with all languages and MCP:

```bash
cargo build --release -p atlas-cli --features mcp
```

> All 14 languages are compiled by default. The `mcp` feature enables the MCP server.

Initialize and index a project before starting MCP (`atlas index` auto-initializes the schema):

```bash
atlas index --project /path/to/project
cd /path/to/project
atlas mcp
```

Atlas MCP uses the client's current working directory. Configure the server without a project path and start the client from the repository you want Atlas to inspect. You can also switch projects at runtime with `project(action="open")`.

MCP client JSON:

```json
{
  "mcpServers": {
    "atlas": {
      "command": "/absolute/path/to/atlas",
      "args": ["mcp"]
    }
  }
}
```

Codex config:

```toml
[mcp_servers.atlas]
command = "/absolute/path/to/atlas"
args = ["mcp"]
enabled = true
```

## MCP tools

The 18 MCP tools use short names (no `atlas_` prefix). The table below is a
task-oriented view, so several rows intentionally share the same tool:

| Task | Tool | Key arguments |
| --- | --- | --- |
| Project overview | `project(action="status")` | none |
| Indexed files | `project(action="files")` | optional `limit`, `language`, `path_prefix` |
| Symbol search | `search` | `query` (required), `scope` required for manifest-only indexes, optional `kind`, `limit`, `background` |
| Symbol details | `symbol(view="detail")` | `symbol` (required), optional `includeCode` |
| Agent context | `symbol(view="context")` | `symbol` (required), optional `includeCode` |
| Symbol usages | `symbol(view="usages")` | `symbol` (required), optional `limit` |
| Call graph | `calls` | `symbol` (required), `direction="incoming\|outgoing\|both"`, optional `depth`, `limit`, `edge_kinds` |
| Symbol exploration | `explore` | `symbol` (required), optional `includeCode` |
| Shortest path | `path` | `from`, `to` (required), optional `max_depth`, `direction`, `prefer_production`, `edge_kinds`, `includeCode` |
| Impact analysis | `impact` | `symbol` (required), optional `depth`, `semantic` |
| File dependencies | `file_dependencies` | `file_path` (required), `direction="incoming\|outgoing\|both"`, optional `limit` |
| Point inspection | `trace(kind="point")` | `file_path` or `file_id`, `line`, `column` |
| Variable origin | `trace(kind="variable")` | `file_path` or `file_id`, `line`, `column`, optional `max_depth` |
| Caller chain | `trace(kind="callers")` | `symbol` (qualified name or SymbolSelector object), optional `max_depth` |
| Forward call trace | `trace(kind="forward")` | `from`, `to` (qualified name or SymbolSelector object), optional `max_depth` |
| Index project | `index` | optional `include`, `exclude`, `analysis`, `background` |
| Open project | `project(action="open")` | `project_path` (required), optional `storage`, `scan_files`, `background` |
| FP dispatch annotations | `fp_dispatches` | `action="add\|list\|delete"` |
| Domain rules | `domain_rules` | `action="add\|list\|delete\|learn"` |
| Background tasks | `tasks` | optional `query_id` |
| Task status | `task_status` | `task_id` (required) |
| Wait for task | `wait_for_task` | `task_id` (required), optional `timeout_secs`, `poll_interval_secs` |
| Resume query | `resume_task` | `query_id` (required) |

## Query tactics

- Start with `search` for names. Use `kind:function`, `kind:class`, or shorter search terms if exact names fail.
- Prefer `file_path` for source-position and file-dependency queries; use `file_id` only when a tool explicitly accepts or returns it.
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

- **No database**: run `atlas index` for the project (auto-initializes schema).
- **Stale result**: run `atlas sync`; restart MCP only if the client does not reconnect or refresh its server process.
- **No symbol found**: retry `search` with shorter names, no kind filter, or larger `limit`.
- **Trace is empty**: check `project(action="status")` and trace capability metadata; the language or construct may have a documented limitation.
- **Huge output**: reduce `limit`, lower graph `depth`, or query a more specific symbol.
