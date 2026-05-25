# Atlas tool guide

## Installation and MCP configuration

Build with all languages and MCP:

```bash
cargo build --release -p atlas-cli --features "all-languages,mcp,bash"
```

> `all-languages` includes TypeScript, JavaScript, Python, Java, C, C++, Go, C#, Rust, PHP, Ruby, Kotlin, ArkTS, and Cangjie. Add `bash` for Bash support (Symbolic tier, no dataflow).

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

| Task | Tool | Key arguments |
| --- | --- | --- |
| Project overview | `atlas_status` | none |
| Indexed files | `atlas_files` | none |
| Capability metadata | `atlas_language_capabilities` | none |
| Symbol search | `atlas_search` | `query`, optional `kind`, `limit` |
| Symbol details | `atlas_symbol` | `qualified_name` |
| Neighbor graph | `atlas_neighbors` | `symbol`, optional `direction`, `depth`, `limit` |
| Callers | `atlas_callers` | `symbol`, optional `limit` |
| Callees | `atlas_callees` | `symbol`, optional `limit` |
| Call graph | `atlas_callgraph` | `symbol`, optional `depth`, `limit` |
| Shortest path | `atlas_path` | `from`, `to`, optional `max_depth` |
| Symbol exploration | `atlas_explore` | `symbol` |
| Impact analysis | `atlas_impact` | `symbol`, optional `depth` |
| Agent context | `atlas_context` | `symbol` |
| Point inspection | `atlas_trace_point` | `file_path` or `file_id`, `line`, `column` |
| Variable origin | `atlas_trace_variable` | `file_path` or `file_id`, `line`, `column`, optional `max_depth` |
| Caller chain | `atlas_trace_caller_path` | `symbol` or `symbol_name`, optional `max_depth` |
| Symbol usages | `usages` | `symbol`, optional `limit` |
| File dependencies | `dependencies` | `file_id`, optional `limit` |
| File dependents | `dependents` | `file_id`, optional `limit` |

## Query tactics

- Start with `atlas_search` for names. Use `kind:function`, `kind:class`, or shorter search terms if exact names fail.
- Convert user-visible file paths to IDs with `atlas_files` only when a tool requires `file_id`.
- Use shallow graph depths first (`depth: 1` or `2`) to avoid noisy results.
- For code-review or refactor questions, combine `atlas_impact` with `usages` and `atlas_context`.
- For debugging value flow, call `atlas_trace_point` first, then `atlas_trace_variable` at the same position.

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
- **No symbol found**: retry `atlas_search` with shorter names, no kind filter, or larger `limit`.
- **Trace is empty**: check `atlas_language_capabilities`; the language or construct may only support symbolic graph queries.
- **Huge output**: reduce `limit`, lower graph `depth`, or query a more specific symbol.
