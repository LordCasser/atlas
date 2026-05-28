# Changelog

All notable changes to Atlas will be documented in this file.

## [1.1.0] — 2026-05-28

### First public release

Atlas is a local-first semantic knowledge graph engine for LLM agents. It parses source code with tree-sitter, stores deterministic code facts in SQLite, and exposes 27 bounded MCP tools plus a CLI for agent-powered codebase navigation.

---

### Core engine

- Deterministic extraction pipeline: tree-sitter parsing → scopes, symbols, references, bindings, dataflow, CFG → SQLite persistence
- blake3-based deterministic IDs for all code facts (14 ID types, 32 bytes each)
- 10-stage reference resolution: scope-local → container-local → same-file → import → include → project-wide → fuzzy (Levenshtein) fallback, each with confidence scoring
- Parallel extraction and resolution via Rayon; thread-local parser pools; batch DB writes
- Incremental sync with two-tier change detection (Git status primary, DB content-hash fallback)

### MCP server

- 27 stdio MCP tools (short names, no `atlas_` prefix):

| Group | Tools |
|---|---|
| Project management | `open_project`, `index`, `status`, `files`, `language_capabilities` |
| Symbol search/detail | `search`, `symbol`, `usages` |
| Graph navigation | `neighbors`, `callers`, `callees`, `callgraph`, `path`, `explore`, `impact` |
| Context | `context` |
| Trace | `trace_point`, `trace_variable`, `trace_caller_path`, `trace_forward` |
| File dependencies | `dependencies`, `dependents` |
| Background tasks | `task_status`, `wait_for_task` |
| C/C++ annotations | `annotate_fp_dispatch`, `list_fp_annotations`, `delete_fp_annotation` |

- Lazy graph initialization: graph-backed tools auto-trigger snapshot load; store-backed tools return immediately
- Background task support: `open_project`, `index`, and `search` support `background=true` with `task_status`/`wait_for_task` polling
- MCP progress notifications via `notifications/progress`; auto-background for long-running tools without progress token
- Runtime project switching via `open_project` (memory mode by default, persistent mode available)

### CLI

- `atlas init` — initialize `.atlas/` directory and SQLite schema
- `atlas index` — full index with glob-based include/exclude filtering
- `atlas sync` — incremental update after file changes
- `atlas status` — file, symbol, edge, database, and capability statistics
- `atlas doctor` — schema, SQLite/FTS5, grammar, and capability readiness check
- `atlas files` — list indexed files with language and parse status
- `atlas search` — FTS5 + LIKE + fuzzy symbol search with kind/language filters
- `atlas context` — Markdown context: callers, callees, imports, file peers
- `atlas trace` — point resolution, variable provenance, caller-path, forward call-chain tracing
- `atlas mcp` — start stdio MCP server (requires `mcp` feature)

### Graph and trace

- In-memory graph snapshots (Arc'd, immutable after load) with confidence-threshold-filtered edges
- BFS/DFS traversal: callers, callees, callgraph, shortest path (with production-file preference), impact analysis, forward frontier
- Location-driven trace engine: `trace_point` (source position → references, symbols, dataflow), `trace_variable` (backward dataflow slicing to value origins), `trace_caller_path` (call chain to farthest caller), `trace_forward` (how A reaches B)
- Cross-function bridging via persisted function summaries (4 tables) for interprocedural parameter↔return reachability
- Lazy dataflow: budget-capped on-demand extraction (25s / 64 units) with artifact caching

### Language support

14 languages at **DataflowFull** capability level:

| Feature group | Languages |
|---|---|
| Default | TypeScript, JavaScript, Python |
| `all-languages` | Java, C, C++, ArkTS, Go, C#, Rust, PHP, Ruby, Kotlin, Cangjie |

- Per-language capability profiles with explicit confidence floors and limitation documentation
- Trace queries return diagnostics rather than silent empty results for unsupported language features

### Architecture

- 14-Cargo-package Rust workspace, edition 2024
- Clean layered stack: `workspace → types → db → extraction + resolution → graph + analysis + search + context → filesync + lazy → atlas-engine (facade) → atlas-cli / atlas-mcp`
- SQLite 22-table schema (WAL mode, `IF NOT EXISTS` idempotency), schema version 1
- Cross-process file locking via `project_metadata` (PID-based with stale-lock stealing)

### Known limitations

- CFG builder has placeholder `walk_if`/`walk_loop` — conditional and loop branches are not traversed
- C/C++ preprocessing not expanded; templates, overloads, and alias analysis not modeled
- Java classpath/Maven/Gradle dependencies not modeled
- Python dynamic/runtime symbol resolution is best-effort
- TypeScript barrel re-exports have limited resolution
- External libraries produce no call edges (only project-internal symbols get call edges)

### Not planned for v1

- Full compiler-grade type checking
- Taint analysis, vulnerability scanning, or SAST product features
- Multi-version source corpus indexing (separate product line)

---

_Atlas v1.0 ships a stable, deterministic knowledge graph foundation. Future releases will focus on performance, correctness improvements, and expanded MCP tool capabilities._
