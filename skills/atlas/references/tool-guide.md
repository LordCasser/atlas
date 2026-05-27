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

All tools use short names (no `atlas_` prefix):

| Task | Tool | Key arguments |
| --- | --- | --- |
| Project overview | `status` | none |
| Indexed files | `files` | none |
| Capability metadata | `language_capabilities` | none |
| Symbol search | `search` | `query`, `scope`, optional `kind`, `limit` |
| Symbol details | `symbol` | `qualified_name` |
| Neighbor graph | `neighbors` | `symbol`, optional `direction`, `depth`, `limit` |
| Callers | `callers` | `symbol`, optional `limit` |
| Callees | `callees` | `symbol`, optional `limit` |
| Call graph | `callgraph` | `symbol`, optional `depth`, `limit` |
| Shortest path | `path` | `from`, `to`, optional `max_depth` |
| Symbol exploration | `explore` | `symbol` |
| Impact analysis | `impact` | `symbol`, optional `depth` |
| Agent context | `context` | `symbol` |
| Point inspection | `trace_point` | `file_path` or `file_id`, `line`, `column` |
| Variable origin | `trace_variable` | `file_path` or `file_id`, `line`, `column`, optional `max_depth` |
| Caller chain | `trace_caller_path` | `symbol` or `symbol_name`, optional `max_depth` |
| Forward call trace | `trace_forward` | `from`, `to`, optional `max_depth` |
| Symbol usages | `usages` | `symbol`, optional `limit` |
| File dependencies | `dependencies` | `file_id`, optional `limit` |
| File dependents | `dependents` | `file_id`, optional `limit` |
| Index project | `index` | optional `include`, `exclude`, `background` |
| Open project | `open_project` | `project_path`, optional `storage`, `scan_files`, `background` |
| Task status | `task_status` | `task_id` |
| Wait for task | `wait_for_task` | `task_id`, optional `timeout_secs` |

## Query tactics

- Start with `search` for names. Use `kind:function`, `kind:class`, or shorter search terms if exact names fail.
- Convert user-visible file paths to IDs with `files` only when a tool requires `file_id`.
- Use shallow graph depths first (`depth: 1` or `2`) to avoid noisy results.
- For code-review or refactor questions, combine `impact` with `usages` and `context`.
- For debugging value flow, call `trace_point` first, then `trace_variable` at the same position.

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
