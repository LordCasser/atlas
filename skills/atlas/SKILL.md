---
name: atlas
description: Semantic code graph engine for local repositories. Indexes 14 languages (TypeScript, JavaScript, Python, Java, C, C++, Go, C#, Rust, PHP, Ruby, Kotlin, ArkTS, Cangjie) via tree-sitter 0.26 and exposes deterministic facts through CLI and MCP. Use for symbol search, call-graph traversal, callers/callees, dependency analysis, variable provenance tracing, caller-path exploration, barrel re-export resolution, or building AI context from indexed codebases.
license: MIT
compatibility: Requires Rust toolchain. Build with `cargo build --release -p atlas-cli --features "all-languages,mcp"`.
metadata:
  version: "1.3.1"
  repository: https://github.com/lordcasser/atlas
---

# Atlas

Use Atlas as the deterministic code-facts layer before reasoning about a repository. Prefer Atlas facts over guessing from filenames or text search. All facts are derived from tree-sitter Concrete Syntax Trees (CST) via language-specific `.scm` queries and AST-walking dataflow builders.

## Language support

14 languages, all at **DataflowFull** level:

| Language | Key capabilities |
|----------|-----------------|
| TypeScript, JavaScript | Symbols, references, imports, scopes, call graph, lexical bindings, intra-procedural dataflow, use-def chains, field access, call arguments, return flow, CFG, interprocedural summaries (ArgToParam + ReturnToCall) |
| Python, Java, C, C++, Go, Rust | Same as above; Python/Java/C/C++/Go/Rust have CFG; C function pointers limited depth 3; C++ templates/overloads not modeled; Rust ReturnToCall gap documented |
| C#, PHP, Ruby, Kotlin, ArkTS, Cangjie | Symbols, references, imports, call graph, lexical bindings, local dataflow, use-def, interprocedural summaries; CFG varies by language (see `project(action="status")` and trace capability metadata) |

All 14 languages compiled by `all-languages`.

## Requirements

A compiled Atlas binary (`atlas`) or an Atlas MCP server, plus a local source checkout. MCP uses the client's current working directory by default; switch repositories with `project(action="open")` when needed. Index the project before relying on search, graph, or trace results.

## Workflow

1. **Confirm the index exists**
   - CLI: `atlas status --project <repo>` or `atlas doctor --project <repo>`
   - MCP: call `project(action="status")`
   - If no `.atlas/atlas.db`, run `atlas init --project <repo>` then `atlas index --project <repo>`, or use MCP `project(action="open", project_path="<repo>", storage="persistent")` followed by `index`

2. **Pick the narrowest query**
   - Symbol lookup: `search` → `symbol`
   - Callers/callees: `calls(direction="incoming")`, `calls(direction="outgoing")`, `calls(depth=2)`
   - Dependencies: `file_dependencies(file_path, direction="incoming")`, `file_dependencies(file_path, direction="outgoing")`
   - Source position: `trace(kind="point")` with `file_path`, `line`, `column`
   - Value origin: `trace(kind="variable")`
   - Caller chain: `trace(kind="callers")`
   - Forward call trace: `trace(kind="forward")`
   - Agent context: `symbol(view="context")`

3. **Respect capability metadata**
   - Call `project(action="status")` or `atlas doctor` when trace precision matters.
   - `partial_result: true` and diagnostics are first-class output; explain limitations.

4. **Refresh after edits**
   - `atlas sync --project <repo>` after modifying files.
   - `atlas index --project <repo>` for full rebuild.

## CLI quick reference

```bash
atlas init --project <repo>
atlas index --project <repo>
atlas sync --project <repo>
atlas status --project <repo>
atlas doctor --project <repo>
atlas search "UserService" --project <repo> --limit 20
atlas context "qualified.symbol.Name" --project <repo>
atlas trace point --project <repo> --file src/app.ts --line 12 --column 18 --json
atlas trace variable --project <repo> --file src/app.ts --line 12 --column 18 --max-depth 30 --json
```

## MCP tools

All 18 tools use short names (no `atlas_` prefix):

| Tool | Purpose | Key arguments |
|------|---------|---------------|
| `project` | Open, inspect, or list files | `action="open\|status\|files"` |
| `index` | Index/re-index active project | optional `include`, `exclude`, `background` |
| `search` | Symbol search by name | `query` (required), optional `scope`, `kind`, `limit`, `background` |
| `symbol` | Symbol details, context, or usages | `symbol` (required), `view="detail\|context\|usages"`, optional `includeCode`, `limit` |
| `calls` | Call graph queries (callers, callees, multi-hop) | `symbol` (required), `direction="incoming\|outgoing\|both"`, optional `depth`, `limit`, `edge_kinds` |
| `explore` | Symbol exploration (depth=1 adjacency) | `symbol` (required), optional `includeCode` |
| `path` | Shortest path between symbols | `from`, `to` (required), optional `max_depth`, `direction`, `edge_kinds`, `includeCode`, `include_roots` |
| `impact` | Bidirectional impact analysis | `symbol` (required), optional `depth`, `semantic` |
| `file_dependencies` | File-level import/include graph | `file_path` (required), `direction="incoming\|outgoing\|both"`, optional `limit` |
| `trace` | Source-level trace (point, variable, forward, callers) | `kind="point\|variable\|forward\|callers"` (required), `file_path`/`file_id`, `line`, `column`, `symbol`, `from`/`to` |
| `lifecycle` | Field lifecycle analysis (C/C++) | `symbol`, `field` (required), optional `include_roots` |
| `branch_diff` | Branch side-effect comparison (C/C++) | `symbol` (required), optional `include_roots` |
| `fp_dispatches` | Function-pointer dispatch annotations | `action="add\|list\|delete"` |
| `domain_rules` | Domain rules for lifecycle analysis | `action="add\|list\|delete\|learn"` |
| `tasks` | List background extraction jobs | optional `query_id` |
| `task_status` | Poll background task progress | `task_id` (required) |
| `wait_for_task` | Block until task completes | `task_id` (required), optional `timeout_secs`, `poll_interval_secs` |
| `resume_task` | Resume a previous partial query | `query_id` (required) |

## Query tactics

- Start with `search` for names. Use `kind:function`, `kind:class`, or shorter terms if exact match fails.
- Prefer shallow graph depths (`depth: 1-2`) to avoid noisy results.
- For barrel re-export chains (`import { X } from './barrel'` where barrel has `export * from './lib'`), Atlas follows the chain to the original definition via `ExportFrom` facts.
- For code review, combine `impact` with `symbol(view="usages")` and `symbol(view="context")`.
- For value flow debugging, call `trace(kind="point")` first, then `trace(kind="variable")` at the same position.

## Trace response handling

All trace tools return an envelope:

- `ok` — whether the query completed.
- `kind` — trace result kind.
- `capability` — language capability metadata (level, features, confidence).
- `partial_result` — whether Atlas returned incomplete evidence.
- `diagnostics` — warnings, unsupported features, lookup ambiguity.
- `result` — trace-specific payload (path steps with kind, file, range, confidence).

When `partial_result: true` or diagnostics present, summarize the evidence and state the limitation.

## Answering rules

- Cite Atlas evidence: symbol names, qualified names, file paths, edge kinds, trace diagnostics.
- Atlas is best-effort static analysis with explicit language capability boundaries. Never claim compiler-grade certainty.
- If Atlas returns nothing, try broader search (shorter name, no kind filter, larger `limit`), then state no indexed fact matched.
- All 14 languages have DataflowFull capability; specific features (CFG, interprocedural) vary — check `project(action="status")` or trace capability metadata.

## References

See [references/tool-guide.md](references/tool-guide.md) for installation, MCP client configuration, and troubleshooting.
