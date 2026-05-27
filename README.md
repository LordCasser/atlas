# Atlas

<p align="center">
  <strong>Local-first semantic knowledge graph engine for LLM agents.</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/language-Rust-orange" alt="Language: Rust">
  <img src="https://img.shields.io/badge/edition-2024-purple" alt="Rust Edition: 2024">
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="License: MIT">
  <img src="https://img.shields.io/badge/MCP-ready-green" alt="MCP ready">
</p>

Atlas parses source code with tree-sitter, stores deterministic code facts in a local SQLite database, and exposes those facts through a CLI and an MCP server. It is built for agents that need reliable codebase context: symbol search, callers/callees, dependency edges, impact analysis, point inspection, and bounded variable/caller tracing.

```text
source code ──parse/extract──▶ .atlas/atlas.db ──query──▶ CLI / MCP tools
            tree-sitter facts     SQLite source of truth      agent context
```

## Table of contents

- [Features](#features)
- [Install](#install)
- [Quick start](#quick-start)
- [CLI](#cli)
- [MCP server](#mcp-server)
- [Architecture](#architecture)
- [Supported languages](#supported-languages)
- [Documentation](#documentation)
- [Development](#development)
- [Known limitations](#known-limitations)
- [License](#license)

## Features

- **Local-first**: writes all index data to `<project>/.atlas/atlas.db`; no cloud service required.
- **Deterministic extraction**: tree-sitter AST queries and stable blake3-based IDs instead of model guesses.
- **Incremental sync**: content-hash based dirty-file detection with Git-aware file discovery.
- **Agent-native MCP**: stdio MCP server exposing 24 bounded tools for search, graph, context, dependencies, trace, background tasks, and project management.
- **Graph + trace queries**: callers, callees, shortest path, impact, source-position lookup, variable origin tracing, and caller-path tracing.
- **Explicit capability boundaries**: language capability metadata and trace diagnostics report partial results instead of silently overclaiming precision.

## Install

### Requirements

- Rust 1.85+ (Rust edition 2024)
- Git, recommended for file discovery (`atlas` falls back to filesystem traversal when needed)

### Build from source

```bash
git clone https://github.com/LordCasser/atlas.git
cd atlas
cargo build --release -p atlas-cli --features "all-languages,mcp"
```

The binary is generated at `target/release/atlas`.

You can also install the local binary into Cargo's bin directory:

```bash
cargo install --path crates/atlas-cli --features "all-languages,mcp"
```

## Quick start

```bash
# Run from your project root — `--project` defaults to `.`
# Create .atlas/ and initialize the SQLite schema
atlas init

# Build the first full index
atlas index

# Inspect index health and database statistics
atlas status
atlas doctor

# Query symbols and context
atlas search "UserService"
atlas context "my.module.UserService"
```

All commands accept `--project <path>` when running from outside the project
directory (supports both relative and absolute paths).

## CLI

| Command | Purpose |
| --- | --- |
| `atlas init` | Create `.atlas/` and initialize the database schema. |
| `atlas index` | Discover and index source files. Supports `--include` and `--exclude` globs. |
| `atlas sync` | Incrementally update the index after file changes. |
| `atlas status` | Show file, symbol, edge, database, and capability statistics. |
| `atlas doctor` | Check schema, SQLite/FTS5, grammar, and capability readiness. |
| `atlas files` | List indexed files with language and parse status. |
| `atlas search <query>` | Search symbols with FTS5, LIKE fallback, fuzzy prefix matching, kind/language filters, and optional JSON output. |
| `atlas context <symbol>` | Build Markdown context around a symbol: callers, callees, imports, and file peers. |
| `atlas trace point` | Resolve a source position to references, symbols, scopes, bindings, and nearby dataflow facts. |
| `atlas trace variable` | Walk backward through dataflow edges from a source position to value origins. |
| `atlas trace caller-path` | Walk backward through call edges to find a caller chain for a function. |
| `atlas mcp` | Start the stdio MCP server. Requires the `mcp` Cargo feature. |

Examples:

```bash
atlas search "kind:function lang:typescript handle*" --limit 20
atlas files
atlas trace point --file src/app.ts --line 12 --column 18 --json
atlas trace variable --file src/app.ts --line 12 --column 18 --max-depth 30 --json
```

## MCP server

Start the server after indexing the target project:

```bash
# From your project root:
atlas init
atlas index
atlas mcp
```

> MCP reads an existing `.atlas/atlas.db`. Re-run `atlas sync` or `atlas index` after code changes.

### Client configuration

`--project` defaults to `.`, so atlas uses the client's working directory.
This enables a **global configuration** that works across all projects without
hardcoding project paths. You can also switch projects at runtime with the
`open_project` MCP tool.

> Some clients (e.g., Claude Desktop) start from unpredictable working
> directories. For those clients, use **project mode** with an explicit
> `--project <path>`.

Config files by client:

| Client | Global config |
|---|---|
| Claude Code | `~/.claude.json` |
| Codex CLI | `~/.codex/config.toml` |
| OpenCode | `~/.config/opencode/opencode.json` |
| Claude Desktop | `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) |
| Cursor | Cursor Settings → MCP → Add new MCP server |

> Claude Code and OpenCode also support project-level config:
> `.claude/settings.local.json` and `.opencode/opencode.json`.

#### Global mode — no `--project`

Recommended for agents that start from the project directory (Claude Code,
Codex CLI, OpenCode). One config works for every project.

Claude Code (`~/.claude.json`):

```json
{
  "mcpServers": {
    "atlas": {
      "command": "/path/to/atlas",
      "args": ["mcp"]
    }
  }
}
```

OpenCode (`~/.config/opencode/opencode.json`):

```json
{
  "mcpServers": {
    "atlas": {
      "command": "/path/to/atlas",
      "args": ["mcp"]
    }
  }
}
```

Codex CLI (`~/.codex/config.toml`):

```toml
[mcp_servers.atlas]
command = "/path/to/atlas"
args = ["mcp"]
enabled = true
```

#### Project mode — explicit `--project`

Use when the client's working directory is unpredictable, or when you want
to lock atlas to a specific codebase.

Claude Desktop:

```json
{
  "mcpServers": {
    "atlas": {
      "command": "/path/to/atlas",
      "args": ["mcp", "--project", "/path/to/project"]
    }
  }
}
```

Claude Code, OpenCode, Cursor — same JSON format as above. Just add
`"--project"` and the path to the `args` array.

Codex CLI (`~/.codex/config.toml`):

```toml
[mcp_servers.atlas]
command = "/path/to/atlas"
args = ["mcp", "--project", "/path/to/project"]
enabled = true
```

### Tool groups

| Group | MCP tools |
| --- | --- |
| Project management | `open_project`, `index`, `status`, `files`, `language_capabilities` |
| Symbol search/detail | `search`, `symbol`, `usages` |
| Graph navigation | `neighbors`, `callers`, `callees`, `callgraph`, `path`, `explore`, `impact` |
| Context | `context` |
| Trace | `trace_point`, `trace_variable`, `trace_caller_path` |
| File dependencies | `dependencies`, `dependents` |
| Background tasks | `task_status`, `wait_for_task` |

> `open_project` supports switching the active project at runtime. It defaults to `storage: "memory"`, `index: false`, and `scan_files: false` for zero-footprint, fast project switching. Use `background: true` for large trees or `index: true`; then call `task_status` or `wait_for_task` with the returned `task_id`.

Trace tools return the `TraceQueryResponse<T>` envelope documented in [`docs/07-trace-contract.md`](docs/07-trace-contract.md): `ok`, `kind`, `capability`, `partial_result`, `diagnostics`, and `result`.

## Architecture

Atlas is a Rust workspace with 13 Cargo packages. The public entry points are `atlas-cli`, `atlas-mcp`, and the `atlas-engine` facade. Engine internals are split by responsibility so extraction, persistence, graph construction, search, context, and trace can evolve independently.

```text
atlas/
├── crates/
│   ├── atlas-cli                 # CLI binary, command dispatch, logging, integration tests
│   ├── atlas-mcp                 # stdio MCP server powered by rmcp + Atlas tool router
│   └── atlas-engine              # public facade crate re-exporting core APIs
│       └── crates/
│           ├── types             # IDs, IR records, language/capability metadata
│           ├── workspace         # project root and source-path abstractions
│           ├── db                # SQLite schema, Store, readers/writers
│           ├── extraction        # tree-sitter frontends, SCM queries, scopes, bindings, dataflow, CFG
│           ├── resolution        # reference/import/include/path-alias resolution
│           ├── graph             # symbol edge builder, graph snapshot, graph traversal engine
│           ├── analysis          # trace engine, variable slicing, caller-path analysis
│           ├── search            # FTS5 + LIKE + fuzzy search and query parsing
│           ├── context           # agent-facing Markdown context builder
│           └── filesync          # file discovery, content hashing, incremental sync, locks
├── docs/                          # architectural and release documentation
├── skills/atlas/                 # Agent Skill for using Atlas
├── Cargo.toml                    # workspace manifest
└── README.md
```

### Data pipeline

```text
1. Discover files
   └─ Git-aware discovery + include/exclude filters
2. Parse and extract
   └─ tree-sitter frontends produce FileFacts: symbols, scopes, refs, imports, callsites, bindings, dataflow, CFG
3. Persist facts
   └─ SQLite tables under .atlas/atlas.db are the source of truth
4. Resolve references
   └─ scope/container/import/include/project-name matching; unresolved facts keep diagnostics instead of failing indexing
5. Build graph
   └─ resolved refs and callsites become symbol_edges; GraphSnapshot accelerates read-only traversal
6. Serve queries
   └─ CLI commands and MCP tools call SearchEngine, GraphEngine, ContextBuilder, and TraceEngine
```

### Dependency direction

```text
atlas-cli ──▶ atlas-engine, atlas-mcp
atlas-mcp ──▶ atlas-engine

atlas-engine facade ──▶ types, workspace, db, extraction, resolution,
                        graph, analysis, search, context, filesync

engine internals stay acyclic:
types/workspace/db ─▶ extraction/resolution/graph/analysis/search/context/filesync ─▶ facade/API
```

### Storage model

Atlas stores index data in `.atlas/atlas.db` (schema version 1). Core tables include:

```text
files              symbols            scopes             references
imports            symbol_edges       callsites          bindings
binding_uses       data_nodes         dataflow_edges     cfg_nodes
cfg_edges          project_metadata   symbols_fts
```

SQLite is the durable source of truth. In-memory graph snapshots are query accelerators and can be rebuilt from the database.

## Supported languages

Default build:

| Language | Extensions | Capability level |
| --- | --- | --- |
| TypeScript | `.ts`, `.tsx` | DataflowFull |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | DataflowFull |
| Python | `.py`, `.pyi`, `.pyx` | DataflowFull |

`all-languages` build:

| Language | Extensions | Capability level |
| --- | --- | --- |
| Java | `.java` | DataflowFull |
| C | `.c`, `.h` | DataflowFull |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx` | DataflowFull |
| ArkTS | `.ets`, `.sts` | DataflowFull via TypeScript grammar |
| Go | `.go` | DataflowFull |
| C# | `.cs` | DataflowFull |
| Rust | `.rs` | DataflowFull |
| PHP | `.php` | DataflowFull |
| Ruby | `.rb` | DataflowFull |
| Kotlin | `.kt`, `.kts` | DataflowFull |
| Cangjie | `.cj`, `.cangjie` | DataflowFull |

Build variants:

```bash
cargo build --release -p atlas-cli
cargo build --release -p atlas-cli --features all-languages
cargo build --release -p atlas-cli --features "all-languages,mcp"
```

## Documentation

Maintained documents:

- [`docs/architecture.md`](docs/architecture.md) — authoritative architecture: constraints, modules, schema, dataflow, capability profiles, design decisions.
- [`docs/requirements.md`](docs/requirements.md) — product scope and acceptance criteria.
- [`docs/roadmap.md`](docs/roadmap.md) — current and future work.
- [`docs/testing.md`](docs/testing.md) — test layers, feature matrix, and release checks.
- [`docs/performance.md`](docs/performance.md) — measured performance baselines.
- [`docs/trace-contract.md`](docs/trace-contract.md) — frozen trace JSON contract and diagnostics model.
- [`skills/atlas/SKILL.md`](skills/atlas/SKILL.md) — Agent Skill for using Atlas from another agent.

## Development

```bash
# Default tests: TypeScript, JavaScript, Python
cargo test

# Full CLI + MCP + all non-experimental language features
cargo test -p atlas-cli --features "all-languages,mcp"

# Build release binary with MCP
cargo build --release -p atlas-cli --features "all-languages,mcp"
```

Conventions:

1. Keep crate dependencies acyclic and aligned with the architecture above.
2. Add or update fixtures when changing extraction, resolution, graph, or trace behavior.
3. Update [`docs/trace-contract.md`](docs/trace-contract.md) and tests when trace response fields or diagnostics change.
4. Update [`docs/architecture.md`](docs/architecture.md) when implemented module boundaries, schema, CLI, MCP, or analysis behavior changes.
5. Keep release-facing documentation in `docs/`; delete obsolete content rather than accumulating stale docs.

## Known limitations

- Atlas performs best-effort semantic analysis, not compiler-grade type checking.
- C/C++ preprocessing is not expanded; include analysis is based on indexed directives and paths.
- Java classpath, Maven, and Gradle resolution are not fully modeled.
- Python dynamic runtime constructs and generated symbols are outside the static extraction model.
- TypeScript barrel/re-export chains use best-effort name fallback rather than a full export graph.
- Dataflow and trace precision varies by language; inspect `atlas doctor` or `language_capabilities` before relying on a trace result.
- MCP serves a local SQLite index; run `atlas sync` or `atlas index` after source changes.

## How tree-sitter powers dataflow extraction

Atlas builds its code facts entirely from tree-sitter's Concrete Syntax Tree (CST). Here is the pipeline from raw source to traceable dataflow:

### 1. Parse → CST

```text
source code
  → tree_sitter::Parser (per-language grammar)
  → tree_sitter::Tree (CST)
```

Tree-sitter is an incremental, error-tolerant parser. Atlas uses **13 language grammars** (TypeScript, Python, Java, C, C++, Go, C#, Rust, PHP, Ruby, Kotlin, ArkTS, Cangjie), each compiled from a `grammar.js` into a parser. Parsing is done per-file via a thread-local `Parser` to avoid allocation overhead.

### 2. Query → captures

```text
CST root node
  → tree_sitter::Query (per-language .scm queries)
  → (capture_name, Node) pairs
```

Four tree-sitter queries run against every file:

| Query | `.scm` file | Captures |
|-------|-----------|----------|
| **definitions** | `definitions.scm` | `(class_declaration) @definition.class`, `(function_declaration) @definition.function`, etc. |
| **references** | `references.scm` | `(call_expression) @reference.call`, `(member_expression) @reference.field`, etc. |
| **imports** | `imports.scm` | `(import_statement) @import`, module path extraction |
| **scopes** | `scopes.scm` | `(function_declaration) @scope`, `(block) @scope`, etc. |

Each capture includes its **byte range** and **source text** from the CST node. Queries are compiled once per language, then executed against every parsed file via `QueryCursor::captures()`.

### 3. Normalize → FileFacts

```text
(capture_name, Node) pairs
  → LanguageAdapter::normalize()
  → Symbol, Reference, Import, ScopeDef (deterministic ID via blake3)
```

Each language has a `LanguageAdapter` that maps tree-sitter capture names to Atlas types. For example, a `@definition.function` capture becomes a `Symbol` with `SymbolKind::Function`, and its qualified name is built by walking `child_by_field_name("name")` up the CST. All IDs are deterministic — the same file always produces the same facts.

### 4. Lexical binding → scope-aware variable resolution

```text
Symbols + Scopes
  → LexicalBinder (walks CST for `(identifier) @binding.use`)
  → BindingDef (declaration sites) + BindingUse (usage sites)
```

The `LexicalBinder` scans every identifier in the AST. For each usage, it walks the scope chain upward to find the nearest enclosing declaration with a matching name. This produces `BindingDef`/`BindingUse` pairs that connect variable uses to their definitions within the same file.

### 5. Dataflow → intra-procedural edges

```text
CST root + Bindings + Scopes
  → DataFlowBuilder (walks AST for assignment, call, field access, return patterns)
  → DataNode + DataFlowEdge
```

The `DataFlowBuilder` does NOT use tree-sitter queries — it walks the CST directly via `Node::child()`, `child_by_field_name()`, and `named_children()`. For each language, it pattern-matches against known AST node types:

| Pattern | AST nodes matched | Produces |
|---------|------------------|----------|
| Assignment | `variable_declaration`, `assignment_expression` | `Assign` edge: RHS → LHS |
| Call arguments | `call_expression` → `arguments` → children | `ArgToCall` edge: arg → call parameter slot |
| Field access | `member_expression` → `property_identifier` | `FieldLoad`/`FieldStore` edges |
| Return values | `return_statement` → child expression | `ReturnValue` edge |
| Destructuring | `pattern_list`, `tuple_pattern`, `object_pattern` | Multi-target `Assign` edges |

`DataNode` records the source location (byte range), kind (Local, Param, Field, CallArg, Return, Expr), and function scope. `DataFlowEdge` connects a source node to a target node with a directed kind and confidence score.

### 6. CFG → control flow (TypeScript/JavaScript only)

```text
CST root (per function)
  → CfgBuilder (walks function body, matching branch/loop/break AST patterns)
  → CfgNode + CfgEdge (Entry → blocks → Exit)
```

CFG construction walks the function AST, identifying control-flow splits (`if_statement`, `switch_case`, `try_statement`, `for_statement`, `while_statement`) and building a graph of basic blocks. Each `CfgNode` records the byte range it covers, and `CfgEdge` connects predecessor → successor.

### 7. Trace → cross-procedural variable provenance

```text
Symbol graph + DataFlow graph + CFG
  → TraceEngine (backward slice from user-specified location)
  → TracePath (step-by-step provenance: kind, range, file, confidence, evidence)
```

The `TraceEngine` combines symbol-level call graphs with intra-procedural dataflow. At call boundaries, `SummaryEdgeProvider` materializes virtual edges (`ArgToParam`, `ReturnToCall`) on-demand, bridging the gap between caller and callee without pre-computing all inter-procedural summaries.

### Where to find the code

| Component | Crate | Key files |
|-----------|-------|-----------|
| Grammar registry | `extraction` | [`grammar.rs`](crates/atlas-engine/crates/extraction/src/grammar.rs) |
| Queries | `extraction` | [`queries/<lang>/*.scm`](crates/atlas-engine/crates/extraction/queries/) |
| Language adapters | `extraction` | [`languages/<lang>.rs`](crates/atlas-engine/crates/extraction/src/languages/) |
| Normalize pipeline | `extraction` | [`extract.rs`](crates/atlas-engine/crates/extraction/src/extract.rs) |
| Query helpers | `extraction` | [`query_helpers.rs`](crates/atlas-engine/crates/extraction/src/query_helpers.rs) |
| Lexical binding | `extraction` | [`lexical_binder.rs`](crates/atlas-engine/crates/extraction/src/lexical_binder.rs) |
| DataFlow builder | `extraction` | [`dataflow_builder.rs`](crates/atlas-engine/crates/extraction/src/dataflow_builder.rs) |
| CFG builder | `extraction` | [`cfg_builder.rs`](crates/atlas-engine/crates/extraction/src/cfg_builder.rs) |
| Capability profiles | `types` | [`capability.rs`](crates/atlas-engine/crates/types/src/capability.rs) |
| Trace engine | `analysis` | [`trace/engine.rs`](crates/atlas-engine/crates/analysis/src/trace/engine.rs) |

## License

MIT. See [`LICENSE`](LICENSE).
