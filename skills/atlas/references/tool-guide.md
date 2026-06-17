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

The 15 MCP tools use short names in the native server. Some MCP client environments add an `atlas_` prefix (e.g., `atlas_search`, `atlas_calls`). This table uses the task-oriented view:

| Task | Tool | Key arguments |
| --- | --- | --- |
| Open project | `project(action="open")` | `project_path` (required), optional `storage` (`auto`/`memory`/`persistent`) |
| Project overview | `project(action="status")` | optional `verbose` |
| Indexed files | `project(action="files")` | optional `limit`, `language`, `path_prefix` |
| Symbol search | `search` | `query` (required), `scope` (required, project-relative dir), optional `kind`, `limit`, `include_roots` |
| Symbol details | `symbol(view="detail")` | `symbol` (required, string or SymbolSelector), optional `file_path`+`line`+`column` for position lookup, `includeCode` |
| Agent context | `symbol(view="context")` | `symbol` (required, string or SymbolSelector), optional `file_path`+`line`+`column`, `includeCode`, `includeFilePeers` |
| Symbol usages | `symbol(view="usages")` | `symbol` (required, string or SymbolSelector), optional `file_path`+`line`+`column`, `limit` |
| Call graph | `calls` | `symbol` (required), `direction` (`incoming`/`outgoing`/`both`; default `both`), optional `depth` (1-5), `limit`, `edge_kinds` |
| Symbol exploration | `explore` | `symbol` (required), optional `source_mode` (`excerpt`/`full`/`none`), `source_lines`, `include_file_context`, `include_recommendations` |
| Shortest path | `path` | `from`, `to` (required), optional `max_depth` (1-10, default 5), `direction`, `prefer_production`, `edge_kinds`, `includeCode` |
| Impact analysis | `impact` | `symbol` (required), optional `depth` (1-5, default 3), `semantic` |
| File dependencies | `file_dependencies` | `file_path` (required), `direction` (`outgoing`/`incoming`/`both`; default `outgoing`), optional `limit`, `analysis` (`manifest`/`structural`) |
| Point inspection | `trace(kind="point")` | `file_path` or `file_id`, `line`, `column` |
| Variable origin | `trace(kind="variable")` | `file_path` or `file_id`, `line`, `column`, optional `max_depth` (default 30) |
| Caller chain | `trace(kind="callers")` | `symbol` (qualified name or SymbolSelector), optional `max_depth` (default 10) |
| Forward call trace | `trace(kind="forward")` | `from`, `to` (qualified name or SymbolSelector), optional `max_depth` (default 20) |
| FP dispatch annotations | `fp_dispatches` | `action` (`add`/`list`/`delete`) |
| Domain rules | `domain_rules` | `action` (`add`/`list`/`delete`/`learn`) |
| Background tasks | `tasks` | optional `query_id` |
| Resume query | `resume_query` | `query_id` (required) |

## Query tactics

- Start with `search` for names. `search` **requires `scope`** — provide a project-relative directory (e.g., `"src"`, `"drivers/net"`). Use `kind: "function"`, `kind: "class"`, or shorter search terms if exact names fail.
- Prefer `file_path` for source-position and file-dependency queries; use `file_id` only when a tool explicitly accepts or returns it.
- Use shallow graph depths first (`depth: 1` or `2`) to avoid noisy results.
- For code-review or refactor questions, combine `impact` with `symbol(view="usages")` and `symbol(view="context")`.
- For debugging value flow, call `trace(kind="point")` first, then `trace(kind="variable")` at the same position.
- For position-based lookup, combine `symbol` with `file_path`, `line`, `column`, and `view` (e.g., `symbol(file_path="src/foo.ts", line=42, column=1, view="context")`).
- If SymbolSelector returns ambiguous with a `file_path` hint, check the error message — invalid `file_path` values are diagnosed inline.

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

- **No database**: run `atlas index --project <repo>` (auto-initializes schema), then restart MCP if needed.
- **Stale results**: run `atlas sync --project <repo>`; restart MCP only if the client does not reconnect or refresh its server process.
- **No symbol found**: retry `search` with shorter names, no kind filter, or larger `limit`.
- **Trace is empty**: check `project(action="status")` and trace capability metadata; the language or construct may have a documented limitation.
- **Huge output**: reduce `limit`, lower graph `depth`, or query a more specific symbol.
