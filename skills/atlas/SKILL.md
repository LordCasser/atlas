---
name: atlas
description: Use Atlas, a local-first semantic code graph engine, to index repositories and answer codebase questions through the atlas CLI or Atlas MCP tools. Use when the task involves symbol search, call graphs, callers/callees, dependency analysis, impact analysis, source-position inspection, variable origin tracing, caller-path tracing, or building reliable agent context from a local codebase.
license: MIT
---

# Atlas

Use Atlas as the deterministic code-facts layer before reasoning about a repository. Prefer Atlas facts over guessing from filenames or text search when answering questions about symbols, calls, dependencies, or variable origins.

## Requirements

Use an Atlas CLI binary or an already configured Atlas MCP server, plus a local source checkout. MCP usage requires the target project to be indexed first.

## Workflow

1. **Confirm the index exists**
   - CLI: run `atlas status --project <repo>` or `atlas doctor --project <repo>`.
   - MCP: call `atlas_status`.
   - If no `.atlas/atlas.db` exists, run `atlas init --project <repo>` then `atlas index --project <repo>`.

2. **Pick the narrowest query**
   - Need a symbol: use `atlas_search`, then `atlas_symbol`.
   - Need local context: use `atlas_context`.
   - Need callers/callees: use `atlas_callers`, `atlas_callees`, or `atlas_callgraph`.
   - Need dependency direction: use `atlas_files` to get `file_id`, then `dependencies` or `dependents`.
   - Need source-position facts: use `atlas_trace_point` with `file_path`, `line`, and `column`.
   - Need value origin: use `atlas_trace_variable`.
   - Need entry/caller chain: use `atlas_trace_caller_path`.

3. **Respect capability metadata**
   - Call `atlas_language_capabilities` or inspect `atlas doctor` when trace precision matters.
   - Treat `partial_result: true` and diagnostics as first-class output; explain limitations instead of hiding them.

4. **Refresh after edits**
   - Run `atlas sync --project <repo>` after modifying files.
   - For large or uncertain changes, run `atlas index --project <repo>` again.
   - MCP reads the existing database; make sure the index is fresh before relying on results.

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

## MCP usage patterns

Use MCP tools directly when they are available in the agent runtime:

```json
{ "query": "UserService", "limit": 10 }
```

with `atlas_search`, then pass the selected `qualified_name` or `symbol` to graph/context tools.

For trace tools, prefer `file_path` over `file_id` unless the user already provided a file ID:

```json
{ "file_path": "src/app.ts", "line": 12, "column": 18, "max_depth": 30 }
```

## Answering rules

- Cite the Atlas evidence you used: symbol names, qualified names, file paths, edge kinds, trace diagnostics, or returned call chains.
- Do not claim compiler-grade certainty. Atlas is best-effort static analysis with explicit language capability boundaries.
- If Atlas returns no result, try a broader search (`limit`, kind-less query, shorter name), then state that no indexed fact matched.
- Keep outputs bounded: ask for specific symbols, shallow graph depth, or limited results before broad traversals.

## References

Read [references/tool-guide.md](references/tool-guide.md) when you need the full MCP tool map, CLI/MCP configuration snippets, or troubleshooting guidance.
