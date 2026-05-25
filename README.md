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
- **Agent-native MCP**: stdio MCP server exposing 20 bounded tools for search, graph, context, dependencies, trace, and project management.
- **Graph + trace queries**: callers, callees, shortest path, impact, source-position lookup, variable origin tracing, and caller-path tracing.
- **Explicit capability boundaries**: language capability metadata and trace diagnostics report partial results instead of silently overclaiming precision.

## Install

### Requirements

- Rust 1.85+ (Rust edition 2024)
- Git, recommended for file discovery (`atlas` falls back to filesystem traversal when needed)

### Build from source

```bash
git clone https://github.com/<your-org>/atlas.git
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
# Create <project>/.atlas/ and initialize the SQLite schema
atlas init --project /path/to/project

# Build the first full index
atlas index --project /path/to/project

# Inspect index health and database statistics
atlas status --project /path/to/project
atlas doctor --project /path/to/project

# Query symbols and context
atlas search "UserService" --project /path/to/project
atlas context "my.module.UserService" --project /path/to/project
```

When running commands from the project root, omit `--project`; it defaults to `.`.

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
atlas files --project .
atlas trace point --file src/app.ts --line 12 --column 18 --json
atlas trace variable --file src/app.ts --line 12 --column 18 --max-depth 30 --json
```

## MCP server

Start the server after indexing the target project:

```bash
atlas init --project /path/to/project
atlas index --project /path/to/project
atlas mcp --project /path/to/project
```

> MCP reads an existing `.atlas/atlas.db`. Re-run `atlas sync` or `atlas index` after code changes.

### Client configuration

Claude Desktop / Cursor-style JSON:

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

Codex CLI (`~/.codex/config.toml`):

```toml
[mcp_servers.atlas]
command = "/absolute/path/to/atlas"
args = ["mcp", "--project", "/absolute/path/to/project"]
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

> `open_project` supports switching the active project at runtime. It defaults to `storage: "memory"` for zero-footprint temporary sessions. See [`docs/architecture.md`](docs/architecture.md) for details.

Trace tools return the `TraceQueryResponse<T>` envelope documented in [`docs/trace-contract.md`](docs/trace-contract.md): `ok`, `kind`, `capability`, `partial_result`, `diagnostics`, and `result`.

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
│           ├── db                # SQLite schema, Store, readers/writers, migrations
│           ├── extraction        # tree-sitter frontends, SCM queries, scopes, bindings, dataflow, CFG
│           ├── resolution        # reference/import/include/path-alias resolution
│           ├── graph             # symbol edge builder, graph snapshot, graph traversal engine
│           ├── analysis          # trace engine, variable slicing, caller-path analysis
│           ├── search            # FTS5 + LIKE + fuzzy search and query parsing
│           ├── context           # agent-facing Markdown context builder
│           └── filesync          # file discovery, content hashing, incremental sync, locks
├── docs/                         # maintained user/contributor documentation
├── docs/archive/                 # completed/superseded development plans and phase logs
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
cfg_edges          project_metadata   schema_versions    symbols_fts
```

SQLite is the durable source of truth. In-memory graph snapshots are query accelerators and can be rebuilt from the database.

## Supported languages

Default build:

| Language | Extensions | Capability level |
| --- | --- | --- |
| TypeScript | `.ts`, `.tsx` | DataflowBasic |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | DataflowBasic |
| Python | `.py`, `.pyi`, `.pyx` | DataflowBasic |

`all-languages` build:

| Language | Extensions | Capability level |
| --- | --- | --- |
| Java | `.java` | DataflowBasic best-effort |
| C | `.c`, `.h` | DataflowBasic best-effort |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx` | DataflowBasic best-effort |
| ArkTS | `.ets`, `.sts` | DataflowBasic best-effort via TypeScript grammar |
| Go | `.go` | DataflowBasic best-effort |
| C# | `.cs` | DataflowBasic best-effort |
| Rust | `.rs` | DataflowBasic best-effort |
| PHP | `.php` | DataflowBasic best-effort |
| Ruby | `.rb` | DataflowBasic best-effort |
| Kotlin | `.kt`, `.kts` | DataflowBasic best-effort |

Experimental features:

| Language | Extensions | Feature |
| --- | --- | --- |
| Bash | `.sh`, `.bash` | `bash` |
| Cangjie | `.cj`, `.cangjie` | `cangjie` |

Build variants:

```bash
cargo build --release -p atlas-cli
cargo build --release -p atlas-cli --features all-languages
cargo build --release -p atlas-cli --features "all-languages,mcp"
cargo build --release -p atlas-cli --features "all-languages,mcp,bash,cangjie"
```

## Documentation

Maintained documents:

- [`docs/architecture.md`](docs/architecture.md) — 完整技术架构详解（中文）：从 tree-sitter 到惰性数据流的演进路径、技术局限、架构权衡。
- [`docs/01-requirements.md`](docs/01-requirements.md) — product scope and acceptance criteria.
- [`docs/02-architecture-constraints.md`](docs/02-architecture-constraints.md) — architectural rules and module boundaries.
- [`docs/03-current-architecture.md`](docs/03-current-architecture.md) — implemented architecture details.
- [`docs/05-roadmap.md`](docs/05-roadmap.md) — current and future work; completed roadmap items are archived.
- [`docs/07-testing-spec.md`](docs/07-testing-spec.md) — test layers, feature matrix, and release checks.
- [`docs/08-performance-baseline.md`](docs/08-performance-baseline.md) — measured performance baselines.
- [`docs/trace-contract.md`](docs/trace-contract.md) — trace JSON contract and diagnostics model.
- [`skills/atlas/SKILL.md`](skills/atlas/SKILL.md) — Agent Skill for using Atlas from another agent.

Current and future work is tracked in [`docs/05-roadmap.md`](docs/05-roadmap.md). Completed or superseded development notes are kept under [`docs/archive/`](docs/archive/) and are not required for normal use.

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
4. Update [`docs/03-current-architecture.md`](docs/03-current-architecture.md) when implemented module boundaries, schema, CLI, MCP, or analysis behavior changes.
5. Keep release-facing documentation in `README.md` and `docs/`; put historical plans in `docs/archive/`.

## Known limitations

- Atlas performs best-effort semantic analysis, not compiler-grade type checking.
- C/C++ preprocessing is not expanded; include analysis is based on indexed directives and paths.
- Java classpath, Maven, and Gradle resolution are not fully modeled.
- Python dynamic runtime constructs and generated symbols are outside the static extraction model.
- TypeScript barrel/re-export chains use best-effort name fallback rather than a full export graph.
- Dataflow and trace precision varies by language; inspect `atlas doctor` or `atlas_language_capabilities` before relying on a trace result.
- MCP serves a local SQLite index; run `atlas sync` or `atlas index` after source changes.

## License

MIT. See [`LICENSE`](LICENSE).
