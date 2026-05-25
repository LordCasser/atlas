---
name: atlas
description: Semantic code graph engine for local repositories. Indexes 15 languages (TypeScript, JavaScript, Python, Java, C, C++, Go, C#, Rust, PHP, Ruby, Kotlin, ArkTS, Cangjie, Bash) via tree-sitter 0.26 and exposes deterministic facts through CLI and MCP. Use for symbol search, call-graph traversal, callers/callees, dependency analysis, variable provenance tracing, caller-path exploration, barrel re-export resolution, or building AI context from indexed codebases.
license: MIT
compatibility: Requires Rust toolchain. Build with `cargo build --release -p atlas-cli --features "all-languages,mcp,bash"`.
metadata:
  version: "1.0"
  repository: https://github.com/lordcasser/atlas
---

# Atlas

Use Atlas as the deterministic code-facts layer before reasoning about a repository. Prefer Atlas facts over guessing from filenames or text search. All facts are derived from tree-sitter Concrete Syntax Trees (CST) via language-specific `.scm` queries and AST-walking dataflow builders.

## Language support

15 languages across two tiers:

| Tier | Languages | Capability |
|------|-----------|------------|
| **DataflowBasic** (13) | TypeScript, JavaScript, Python, Java, C, C++, Go, C#, Rust, PHP, Ruby, Kotlin, ArkTS | Symbols, references, imports, scopes, call graph, lexical bindings, intra-procedural dataflow (assign, call args, field access, return), use-def chains. TS/JS also have CFG. Trace variable/caller path. |
| **Symbolic** (2) | Cangjie, Bash | Symbols, references. Cangjie has imports. Bash has low-confidence call graph. No dataflow/trace. |

All 13 DataflowBasic + Cangjie are compiled by `all-languages`. Bash requires explicit `bash` feature.

## Requirements

A compiled Atlas binary (`atlas`) or an Atlas MCP server, plus a local source checkout. MCP requires the project to be indexed first.

## Workflow

1. **Confirm the index exists**
   - CLI: `atlas status --project <repo>` or `atlas doctor --project <repo>`
   - MCP: call `status`
   - If no `.atlas/atlas.db`, run `atlas init --project <repo>` then `atlas index --project <repo>`

2. **Pick the narrowest query**
   - Symbol lookup: `search` → `symbol`
   - Callers/callees: `callers`, `callees`, `callgraph`
   - Dependencies: `dependencies`, `dependents`
   - Source position: `atlas_trace_point` with `file_path`, `line`, `column`
   - Value origin: `atlas_trace_variable`
   - Caller chain: `atlas_trace_caller_path`
   - Agent context: `context`

3. **Respect capability metadata**
   - Call `atlas_language_capabilities` or `atlas doctor` when trace precision matters.
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

| Tool | Purpose | Key arguments |
|------|---------|---------------|
| `status` | Project statistics | none |
| `files` | Indexed file list | none |
| `atlas_language_capabilities` | Per-language capability profiles | none |
| `search` | Symbol search by name | `query`, optional `kind`, `limit` |
| `symbol` | Symbol details | `qualified_name` |
| `neighbors` | Symbol graph neighborhood | `symbol`, optional `direction`, `depth`, `limit` |
| `callers` | Incoming call edges | `symbol`, optional `limit` |
| `callees` | Outgoing call edges | `symbol`, optional `limit` |
| `callgraph` | Call graph sub-graph | `symbol`, optional `depth`, `limit` |
| `path` | Shortest path between symbols | `from`, `to`, optional `max_depth` |
| `explore` | Symbol structure exploration | `symbol` |
| `impact` | Impact analysis (what depends on) | `symbol`, optional `depth` |
| `context` | Agent context snippet | `symbol` |
| `atlas_trace_point` | Source-position inspection | `file_path` or `file_id`, `line`, `column` |
| `atlas_trace_variable` | Variable provenance trace | `file_path` or `file_id`, `line`, `column`, optional `max_depth` |
| `atlas_trace_caller_path` | Caller chain exploration | `symbol` or `symbol_name`, optional `max_depth` |
| `usages` | Symbol usage sites | `symbol`, optional `limit` |
| `dependencies` | File dependencies | `file_id`, optional `limit` |
| `dependents` | File reverse dependencies | `file_id`, optional `limit` |
| `index` | Index a project | `project` |
| `open_project` | Open/switch project | `project` |

## Query tactics

- Start with `search` for names. Use `kind:function`, `kind:class`, or shorter terms if exact match fails.
- Prefer shallow graph depths (`depth: 1-2`) to avoid noisy results.
- For barrel re-export chains (`import { X } from './barrel'` where barrel has `export * from './lib'`), Atlas follows the chain to the original definition via `ExportFrom` facts.
- For code review, combine `impact` with `usages` and `context`.
- For value flow debugging, call `atlas_trace_point` first, then `atlas_trace_variable` at the same position.

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
- For languages at Symbolic tier (Cangjie, Bash), trace is unsupported. Use only symbol/reference facts.

## References

See [references/tool-guide.md](references/tool-guide.md) for installation, MCP client configuration, and troubleshooting.
