---
name: atlas
description: Semantic code graph engine for local repositories. Indexes 14 languages (TypeScript, JavaScript, Python, Java, C, C++, Go, C#, Rust, PHP, Ruby, Kotlin, ArkTS, Cangjie) via tree-sitter 0.26. Exposes deterministic facts through 15 MCP tools: symbol search, call-graph traversal (callers/callees, multi-hop), dependency analysis, variable provenance tracing, shortest-path between symbols, impact analysis, C/C++ field lifecycle analysis, branch-diff comparison, function-pointer dispatch annotation. Use for understanding code structure, tracing data/call flow, reviewing change impact, or building AI context from indexed codebases. Prefer over text search or filename guessing.
license: MIT
compatibility: Requires Rust toolchain and a local checkout of the target repository. Build with `cargo build --release -p atlas-cli --features mcp`. Index the project via CLI (`atlas index --project <repo>`) before starting MCP.
metadata:
  version: "1.5.1"
  repository: https://github.com/lordcasser/atlas
---

# Atlas

Use Atlas as the deterministic code-facts layer before reasoning about a repository. Prefer Atlas facts over guessing from filenames or text search. All facts are derived from tree-sitter Concrete Syntax Trees (CST) via language-specific `.scm` queries and AST-walking dataflow builders.

## Language support

14 languages compile by default. Capability profiles report **DataflowFull** as the overall level, but individual feature support varies by language; always check `project(action="status")`, `atlas doctor`, or trace capability metadata before making precision claims.

| Language | Key capabilities |
|----------|-----------------|
| TypeScript, JavaScript | Symbols, references, imports, scopes, call graph, lexical bindings, intra-procedural dataflow, use-def chains, field access, call arguments, return flow, CFG, interprocedural summaries (ArgToParam + ReturnToCall) |
| Python, Java, C, C++, Go, Rust | Same as above; Python/Java/C/C++/Go/Rust have CFG; C function pointers limited depth 3; C++ templates/overloads not modeled; Rust ReturnToCall gap documented |
| C#, PHP, Ruby, Kotlin, ArkTS, Cangjie | Symbols, references, imports, call graph, lexical bindings, local dataflow, use-def, interprocedural summaries; CFG varies by language (see `project(action="status")` and trace capability metadata) |

## Requirements

A compiled Atlas binary (`atlas`) or an Atlas MCP server, plus a local source checkout. MCP uses the client's current working directory by default; switch repositories with `project(action="open")`. **Index the project via CLI before starting MCP** — `atlas index --project <repo>` auto-initializes the schema and builds the database.

## Workflow

1. **Confirm the index exists**
   - CLI: `atlas status --project <repo>` or `atlas doctor --project <repo>`
   - MCP: `project(action="status")`
   - If no `.atlas/atlas.db` exists, run `atlas index --project <repo>` (CLI), then restart MCP if needed.

2. **Pick the narrowest query**
   - Symbol lookup: `search` → `symbol`
   - Callers/callees: `calls(direction="incoming")`, `calls(direction="outgoing")`, `calls(depth=2)`
   - Dependencies: `file_dependencies(file_path, direction="incoming")`, `file_dependencies(file_path, direction="outgoing")`
   - Source position: `trace(kind="point")` with `file_path`, `line`, `column`
   - Value origin: `trace(kind="variable")`
   - Caller chain: `trace(kind="callers")`
   - Forward call trace: `trace(kind="forward")`
   - Shortest path: `path(from="X", to="Y")`
   - Impact analysis: `impact(symbol="X")`
   - Agent context: `symbol(view="context")`
   - Symbol exploration: `explore(symbol="X")`

3. **Respect capability metadata**
   - Call `project(action="status")` or `atlas doctor` when trace precision matters.
   - `partial_result: true` and diagnostics are first-class output; explain limitations.

4. **Refresh after edits**
   - `atlas sync --project <repo>` after modifying files.
   - `atlas index --project <repo>` for full rebuild.

## CLI quick reference

```bash
atlas index --project <repo>        # auto-initializes schema + indexes
atlas sync --project <repo>         # incremental update
atlas status --project <repo>       # index health overview
atlas doctor --project <repo>       # detailed diagnostics
atlas                                # from project root: launch TUI (indexes first if needed)
```

## MCP tools

The 15 MCP tools use short names in the native server. Note: some MCP client environments add an `atlas_` prefix (e.g., `atlas_search`, `atlas_calls`). Use the name your environment exposes.

### Tools reference

| Tool | Purpose | Required params | Key optional params |
|------|---------|----------------|---------------------|
| `project` | Open project, check status, list files | — | `action` (`open`/`status`/`files`; default `status`), `project_path` (required with `open`), `storage` (`auto`/`memory`/`persistent`), `verbose`, `limit`, `language`, `path_prefix` |
| `search` | Symbol search by name within a directory scope | `query`, `scope` | `kind` (e.g., `function`, `class`), `limit` (default 20, max 200), `include_roots` |
| `symbol` | Symbol details, rich context, or usages | `symbol` (string or SymbolSelector) | `file_path`+`line`+`column` for position-based lookup, `view` (`detail`/`context`/`usages`; default `detail`), `includeCode`, `includeFilePeers`, `limit` (usages only), `include_roots` |
| `calls` | Call graph: callers, callees, multi-hop | `symbol` | `direction` (`incoming`/`outgoing`/`both`; default `both`), `depth` (1-5, default 1), `limit`, `edge_kinds` (default `["calls","instantiates","implements"]`; use `["*"]` for all) |
| `explore` | Symbol dossier: source, call evidence, relations, file context | `symbol` | `scope` (directory), `source_mode` (`excerpt`/`full`/`none`), `source_lines` (default 40), `evidence_limit` (default 5), `relation_limit` (default 20), `peer_limit` (default 12), `include_file_context`, `include_recommendations` |
| `path` | Shortest path between two symbols through the graph | `from`, `to` | `max_depth` (1-10, default 5), `direction` (`outgoing`/`incoming`/`both`; default `outgoing`), `prefer_production`, `edge_kinds` (default `["calls","instantiates","implements","registers_callback"]`), `includeCode`, `include_roots` |
| `impact` | Bidirectional impact analysis (what would break?) | `symbol` | `depth` (1-5, default 3), `semantic` (include lifecycle invariants and branch diffs) |
| `file_dependencies` | File-level import/include graph | `file_path` | `direction` (`outgoing`/`incoming`/`both`; default `outgoing`), `limit` (default 50), `analysis` (`manifest`/`structural`; default `manifest`) |
| `trace` | Source-level trace: point resolution, variable provenance, forward/caller chains | — | `kind` (`point`/`variable`/`forward`/`callers`; default `point`), `file_path`/`file_id`, `line`, `column`, `symbol`, `from`/`to`, `max_depth`, `include_roots` |
| `lifecycle` | C/C++ field lifecycle through CFG (allocate → use → free) | `symbol`, `field` | `include_roots` |
| `branch_diff` | Compare branch side effects within a function (C/C++) | `symbol` | `include_roots` |
| `domain_rules` | Manage lifecycle domain rules (alloc/free/owned patterns) | — | `action` (`add`/`list`/`delete`/`learn`), `rule_kind`, `pattern`, `rule_id`, `source`, `confidence` |
| `fp_dispatches` | C/C++ function-pointer dispatch annotations | — | `action` (`add`/`list`/`delete`), `field_qname`, `target_qname`, `annotation_id`, `confidence` |
| `tasks` | List background extraction/lazy-refinement jobs | — | `query_id` |
| `resume_query` | Re-run a previous query with enhanced results after lazy refinement | `query_id` | — |

### Trace `kind` parameter details

| `kind` | Purpose | Required params | Default `max_depth` |
|--------|---------|----------------|---------------------|
| `point` | Resolve a source position to its enclosing symbol, scope, and callsite | `file_path`/`file_id`, `line`, `column` | — |
| `variable` | Trace where a variable's value comes from (backward intra-procedural dataflow) | `file_path`/`file_id`, `line`, `column` | 30 |
| `forward` | Trace the forward call chain from source to target | `from`, `to` | 20 |
| `callers` | Trace how a function gets invoked (backward call chain) | `symbol` | 10 |

## Symbol Selector

All tools that accept symbol references (`calls`, `impact`, `path`, `explore`,
`symbol`, `trace`, `usages`) accept two input formats:

1. **String** — qualified symbol name (e.g., `"atlas_engine::Engine"`):
   - Graph tools (`calls`, `impact`, `path`) auto-aggregate all matching symbols
   - Detail tools (`symbol`, `explore`) return a candidate list

2. **SymbolSelector object** — structured selector with fault-tolerant scoring:
   ```json
   {
     "qualified_name": "turn",
     "file_path": "src/foo.ts",
     "line": 42,
     "kind": "function",
     "language": "typescript"
   }
   ```
   - Only `qualified_name` is required
   - Other fields are hints for ranking — wrong values never block correct matches
   - `symbol_ref` from `search` or `symbol` results can be reused directly

## Query tactics

- Start with `search` for names. `search` **requires `scope`** — provide a project-relative directory (e.g., `"src"`, `"drivers/net"`). Use `kind: "function"`, `kind: "class"`, or shorter terms if exact match fails.
- Prefer shallow graph depths (`depth: 1-2`) to avoid noisy results.
- For barrel re-export chains (`import { X } from './barrel'` where barrel has `export * from './lib'`), Atlas follows the chain to the original definition via `ExportFrom` facts.
- For code review, combine `impact` with `symbol(view="usages")` and `symbol(view="context")`.
- For position-based symbol lookup: `symbol(file_path="src/foo.ts", line=42, column=1, view="context")`.
- For ambiguous SymbolSelector results, check the error message for `file_path` diagnostics — invalid `file_path` hints are reported inline.
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
- Treat `DataflowFull` as an overall capability tier, not a promise that every feature bit is present for every language. Specific features such as CFG and summaries vary; check `project(action="status")` or trace capability metadata.

## References

See [references/tool-guide.md](references/tool-guide.md) for installation, MCP client configuration, and troubleshooting.
