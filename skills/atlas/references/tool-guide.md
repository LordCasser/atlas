# Atlas tool guide (MCP-first)

Loaded on demand from [SKILL.md](../SKILL.md).

## Agent vs operator

| Role | Allowed |
|------|---------|
| **Agent (this skill)** | MCP only: `project` open/status/files, search, graph, trace, resume, … |
| **Operator / human** | May run CLI `atlas index` / `sync` offline if they want a project-wide cache |

**Agents must not run `atlas index` (any analysis mode) or whole-tree `atlas sync`.**
On large trees that blocks the session. Focus MCP already builds **local**
structural + on-demand dataflow (Focus materialize, not a separate product) for
the investigation neighborhood.

## Installation (host setup)

```bash
cargo build --release -p atlas-cli --features mcp
# binary: target/release/atlas
```

Start MCP:

```bash
atlas mcp
```

There is **no** `storage` parameter on open (single persistent SQLite only).

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

Codex:

```toml
[mcp_servers.atlas]
command = "/absolute/path/to/atlas"
args = ["mcp"]
enabled = true
```

Tool names are short (`search`, `calls`, …). Some hosts use `atlas_search`, etc.

## MCP open-first

Server starts **unopened**. Always:

```
project(action="open", project_path=...)
```

Open creates/opens `project/.atlas/atlas.db`. Subsequent scoped tools fill facts.
**No pre-index required.**

## Focus + local dataflow (why agents skip full Index)

Without a finalized full CLI cache:

| Layer | How it appears |
|-------|----------------|
| Structural (seed/closure) | On-demand Focus extract + scoped resolve/edges |
| Import neighbors | Often `resolution_symbols` only until needed |
| Dataflow / CFG | Focus on-demand unit extract on `trace(variable)`, lifecycle, branch_diff, etc. |
| Refinement | `analysis.retry_after_ms` → `resume_query` / `tasks` |

Use **resume**, not full index, when the first answer is thin.

## Task → MCP tool

| Task | Tool | Key arguments |
|------|------|----------------|
| Open | `project(action="open")` | `project_path` required |
| Status / files | `project(action="status"\|"files")` | optional filters |
| Search | `search` | `query` + **`scope`** |
| Detail / context / usages | `symbol` | `view` |
| Calls | `calls` | incoming/outgoing are fixed one hop; only `direction=both` honors depth |
| Explore | `explore` | `symbol` |
| Path / impact | `path`, `impact` | — |
| File deps | `file_dependencies` | manifest reads stored facts only; structural requests CallGraph Focus |
| Point / variable / callers / forward | `trace` | see SKILL |
| C/C++ lifecycle / branch | `lifecycle`, `branch_diff` | — |
| Refine | **`resume_query`**, `tasks` | `query_id` |

## Cold Focus checklist

1. First hit may need a larger closure; MCP waits up to 18 seconds.
2. If `retry_after_ms` → the response is ticket-only; wait → `resume_query`.
3. Sparse callers ≠ zero callers in the repo.
4. Need more signal → narrower scope, better selector, `include_roots`, resume.
5. **Do not** `atlas index` the monorepo from the agent.

## Response fields

**Most tools:** `query_id`, `analysis` (`scope`, `summary`, `basis`, optional `retry_after_ms`),
optional `gaps`, `warnings`, `note`, `coverage_counts`.

Non-terminal responses contain only `status=in_progress`, the query ticket,
required analysis level, pending reason, and retry timing. Do not consume or
expect provisional result fields.

`status=failed` is also result-free. Read `pending.detail` and re-run the
original tool call; do not interpret absence of `retry_after_ms` as success.
`tasks(query_id=...)` reports the same failure as `status=failed` with
`pending_jobs=0` and no retry timer.

**Trace:** also `ok`, `kind`, `capability`, `partial_result`, `diagnostics`, optional
`lazy_summary`, `result`.

Terminal: no `retry_after_ms` and no `gaps` → complete for **current** scope;
no retry with `gaps` → terminal limited result.

Handlers enforce advertised depth/collection maxima even if the MCP host skips
schema validation. Treat `returned`, `truncated`, and per-field totals as part of
the answer contract; narrow the query rather than trying to request an unbounded
payload.

## Troubleshooting (agent)

| Symptom | Action |
|---------|--------|
| No active project | `project(action="open", ...)` |
| No / few hits | Broader `search` **within scope**; fix path; `resume_query` |
| Thin callers | `resume_query`; tighten symbol; do **not** full-index |
| Trace partial | With retry: consume only the ticket and resume. Without retry: read terminal capability/diagnostics. |
| Corrupt DB | Tell user to delete `project/.atlas/` and re-open MCP (human may reindex offline) |
| Huge output | Lower `limit` / `depth` |

## Operator-only CLI (not for agents)

Humans maintaining a durable full-repo cache **outside** the agent loop may run:

```bash
atlas index --project <repo> --analysis structural   # or full
atlas sync --project <repo>
atlas status|doctor --project <repo>
```

Do **not** instruct the agent to run these during MCP investigation of large trees.
If a full cache already exists (`last_index_time` / rich status), MCP simply reuses it.

## Build note

`cargo build --release -p atlas-cli --features mcp` · Rust 1.88+ (edition 2024).
