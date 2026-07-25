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

Atlas parses source code with tree-sitter, stores deterministic code facts in a local SQLite database, and exposes those facts through an interactive TUI, a CLI, and an MCP server. It is built for agents and developers that need reliable codebase context: symbol search, callers/callees, dependency edges, impact analysis, point inspection, bounded variable and caller tracing, forward call-chain queries, and C/C++ function-pointer dispatch annotations.

```text
source code ──parse/extract──▶ .atlas/atlas.db ──query──▶ TUI / CLI / MCP
            tree-sitter facts     SQLite source of truth      agent & developer context
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
- **Interactive TUI**: keyboard-driven Ratatui workbench with symbol search, detail tabs, caller trace, typed parameter forms backed by the same analysis handlers as MCP, and human-oriented result views. Code facts, source, paths, and rules are presented directly; capability, confidence, coverage, and refinement state live in an adaptive HUD. Raw JSON remains available with `r` for auditing. Context and resumable query IDs are injected automatically. Bare `atlas` bootstraps the default structural index when needed.
- **Agent-native MCP**: stdio MCP server exposing 15 bounded tools for open-first scoped search, graph, dependencies, trace, semantic analysis, background work visibility, and project management.
- **Symbol-scoped cold start**: focus expansion follows relevant call/type symbols rather than every peer in a source file. Import/include neighbors stay at the lightweight resolution-symbol layer until a verified relationship requires structural facts; background success and failure both converge to an explicit terminal response.
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
cargo build --release -p atlas-cli --features mcp
```

The binary is generated at `target/release/atlas`.

You can also install the local binary into Cargo's bin directory:

```bash
cargo install --path crates/atlas-cli --features mcp
```

### Release builds

Atlas releases are distributed as source plus release binaries. The GitHub
release workflow builds `atlas-cli` with `--features mcp`, so release binaries
include the CLI, TUI, and MCP server.

Repeatable local release build:

```bash
cargo build --release -p atlas-cli --features mcp
```

Release profile choices are defined in `Cargo.toml`: `opt-level=3`, fat LTO,
single codegen unit, and stripped binaries.

Published binary matrix:

| Platform | Target | Asset |
| --- | --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-musl` | `atlas-linux-x86_64.tar.gz` |
| Linux arm64 | `aarch64-unknown-linux-musl` | `atlas-linux-arm64.tar.gz` |
| Linux riscv64 | `riscv64gc-unknown-linux-gnu` | `atlas-linux-riscv64.tar.gz` |
| macOS arm64 | `aarch64-apple-darwin` | `atlas-macos-arm64.tar.gz` |
| Windows x86_64 | `x86_64-pc-windows-msvc` | `atlas-windows-x86_64.zip` |
| Windows arm64 | `aarch64-pc-windows-msvc` | `atlas-windows-arm64.zip` |

Linux musl binaries are checked for static linking in CI. Source builds require
Rust 1.85+; release CI uses the stable Rust toolchain for the configured target.

## Quick start

```bash
# Run from your project root

# Auto-initialize the SQLite schema and build the index
atlas index

# Check project health
atlas status
atlas doctor

# Launch the interactive TUI (search, detail, trace, MCP-backed analysis palette)
atlas
```

All subcommands accept `--project <path>` when running from outside the
project directory (supports both relative and absolute paths). Bare `atlas`
uses the current directory and does not accept `--project`; run it from the
project root. The MCP server uses the client's current working directory.

## CLI

| Command | Purpose |
| --- | --- |
| `atlas` (no subcommand) | Launch the interactive TUI: symbol search, detail tabs, caller trace, and `:` analysis command palette. If no usable `.atlas/atlas.db` exists, creates one, runs the default structural index, then starts the TUI. |
| `atlas index` | Auto-initialize `.atlas/` schema, then discover and index source files. Supports `--include`, `--exclude`, `--scope`, and `--analysis` (manifest \| structural \| full). |
| `atlas sync` | Incrementally update the index after file changes. Supports `--analysis`. |
| `atlas status` | Show file, symbol, edge, database, and capability statistics. |
| `atlas doctor` | Check schema, SQLite/FTS5, grammar, and capability readiness. |
| `atlas files` | List indexed files with language and parse status. |
| `atlas mcp` | Start the stdio MCP server. Requires the `mcp` Cargo feature. |

## MCP server

The MCP server is open-first: call `project(action="open")` to activate a
project, then use scoped tools to trigger focus-driven extraction for the code
needed by the current investigation. Explicit project-wide indexing is CLI-only:

```bash
# From your project root:
atlas mcp
# optional, outside MCP, when you want a project-wide cache:
atlas index --analysis full
```

> MCP opens `project/.atlas/atlas.db` and reuses current-schema facts when available.
> Without a full CLI-built index, scoped queries expose bounded refinement through
> `analysis.retry_after_ms`, `query_id`, `tasks`, and `resume_query`; terminal
> limitations are returned as structured `gaps`.

### Client configuration

Atlas MCP uses the client's current working directory. Configure the MCP server
without a project path, and start the client from the repository you want Atlas
to inspect. You can also switch projects at runtime with the `project` MCP tool
using `action: "open"`.

Config files by client:

| Client | Global config | Project config |
|---|---|---|
| Claude Code | `~/.claude.json` | `.claude/settings.local.json` |
| Codex CLI | `~/.codex/config.toml` | - |
| OpenCode | `~/.config/opencode/opencode.json` | `opencode.json` in the project root |
| Claude Desktop | `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) | - |
| Cursor | Cursor Settings -> MCP -> Add new MCP server | `.cursor/mcp.json` |
| Grok | `~/.grok/config.toml` | `.grok/config.toml` |

> Claude/Cursor-style clients use `mcpServers` with `command` and `args`.
> OpenCode uses its own `mcp` object: each server is `type: "local"` and
> `command` is a single array containing the executable and arguments.

#### MCP server config

Use the same no-project configuration for every repository.

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
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "atlas": {
      "type": "local",
      "command": ["/path/to/atlas", "mcp"],
      "enabled": true
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

Grok (`~/.grok/config.toml`):

```toml
[mcp_servers.atlas]
command = "/path/to/atlas"
args = ["mcp"]
enabled = true
```

### Tool groups

| Group | MCP tools |
| --- | --- |
| Project management | `project` |
| Symbol search/detail | `search`, `symbol` |
| Graph navigation | `calls`, `path`, `explore`, `impact` |
| Trace | `trace` |
| File dependencies | `file_dependencies` |
| Semantic analysis | `lifecycle`, `branch_diff`, `domain_rules` |
| Background work | `tasks`, `resume_query` |
| FP dispatch (C/C++) | `fp_dispatches` |

Primary required arguments:

| Tool | Required arguments |
| --- | --- |
| `project` | none; `project_path` is required when `action="open"` |
| `search` | `query`, `scope` |
| `symbol` | `symbol` |
| `calls` | `symbol` |
| `explore` | `symbol` |
| `path` | `from`, `to` |
| `impact` | `symbol` |
| `file_dependencies` | `file_path` |
| `trace` | none at schema level; `kind` defaults to `point`, and each trace kind validates its own position or symbol arguments |
| `lifecycle` | `symbol`, `field` |
| `branch_diff` | `symbol` |
| `domain_rules` | none; actions validate their own required fields |
| `fp_dispatches` | none; actions validate their own required fields |
| `tasks` | none |
| `resume_query` | `query_id` |

> `project(action="open")` supports switching the active project at runtime.
> It opens the persistent `project/.atlas/atlas.db`; `project` activates a
> project but never scans or indexes the whole tree.

Trace tools return the `TraceQueryResponse<T>` envelope documented in [`docs/trace-contract.md`](docs/trace-contract.md): `ok`, `kind`, `capability`, `partial_result`, `diagnostics`, and `result`.

## Architecture

Atlas is a Rust workspace with 16 Cargo packages. The public entry points are `atlas-cli` (CLI + TUI), `atlas-mcp`, and the `atlas-engine` facade. Engine internals are split by responsibility so extraction, persistence, graph construction, search, context, dossier assembly, focus scheduling, and trace can evolve independently. The current storage contract is Schema V3: one persistent SQLite database plus bounded focus extraction, not a second application-level cache store. CFG nodes persist managed-resource lexical ownership so path-isolated scope exits survive indexing.

For a cold scoped query, Focus keeps two distinct boundaries: relevant files receive
structural extraction, while import/include dependencies normally receive only
`resolution_symbols`. Scoped resolution is rerun as that boundary grows, and call/type
expansion proceeds from relevant symbol IDs to a bounded fixed point. Files are counted
as covered only after facts are built or reused successfully.

Lazy cache reuse also checks structural invariants that a content hash cannot express.
In particular, stale multiline type ranges produced by older extractor semantics are
rebuilt on demand across supported brace-based languages, including
struct/class/union/interface/trait/enum scopes, so an existing partial index does not
permanently preserve a one-line type definition.

Native TUI search is store-backed and available immediately, even before the in-memory
graph snapshot is ready. The first graph-backed detail view loads that snapshot through
the existing background job system, keeping the terminal event loop responsive.

Graph queries synchronously materialize only their exact seed and return bounded facts;
requested multi-hop closure expansion is tracked as resumable background work. Function-
local semantic queries stay local: lifecycle composes CFG and dataflow effects at query
time and can track both fields and local C/C++ resources, including common Linux kernel
allocators and deallocators.

<p align="center">
  <img src="docs/architecture.svg" alt="Atlas Architecture" width="800">
</p>

```text
atlas/
├── crates/
│   ├── atlas-cli                 # CLI binary + TUI (ratatui) + command dispatch
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
│           ├── domain_rules      # domain-specific semantic rules and rule learning
│           ├── search            # FTS5 + LIKE + fuzzy search and query parsing
│           ├── context           # agent-facing Markdown context builder
│           ├── filesync          # file discovery, content hashing, incremental sync, locks
│           └── lazy              # on-demand dataflow job planning and loading
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
   └─ TUI, CLI commands, and MCP tools call SearchEngine, GraphEngine, ContextBuilder, and TraceEngine
```

### Dependency direction

```text
atlas-cli ──▶ atlas-engine, atlas-mcp
atlas-mcp ──▶ atlas-engine

atlas-engine facade ──▶ types, workspace, db, extraction, resolution,
                        graph, analysis, domain_rules, search, context,
                        filesync, lazy

engine internals stay acyclic:
types/workspace/db ─▶ extraction/resolution/graph/analysis/domain_rules/search/context/filesync/lazy ─▶ facade/API
```

### Storage model

Atlas stores index data in `.atlas/atlas.db` (schema version 3). Core tables include:

```text
files                    symbols            scopes               references
imports                  symbol_edges       callsites            bindings
binding_uses             data_nodes         dataflow_edges       cfg_nodes
cfg_edges                function_summaries summary_param_reaches summary_return_sources
summary_call_arg_sources extraction_state   extraction_jobs      project_metadata
symbols_fts              function_pointer_annotations
```

SQLite is the durable source of truth. In-memory graph snapshots are query accelerators and can be rebuilt from the database.

## Supported languages

All 14 languages are compiled by default:

| Language | Extensions | Capability level |
| --- | --- | --- |
| TypeScript | `.ts`, `.tsx` | DataflowInterproc |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | DataflowInterproc |
| Python | `.py`, `.pyi`, `.pyx` | DataflowInterproc |
| Java | `.java` | DataflowInterproc |
| C | `.c`, `.h` | DataflowInterproc |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx` | DataflowInterproc |
| ArkTS | `.ets`, `.sts` | DataflowInterproc via TypeScript grammar |
| Go | `.go` | DataflowInterproc |
| C# | `.cs` | DataflowInterproc |
| Rust | `.rs` | DataflowInterproc |
| PHP | `.php` | DataflowInterproc |
| Ruby | `.rb` | DataflowInterproc |
| Kotlin | `.kt`, `.kts` | DataflowInterproc |
| Cangjie | `.cj`, `.cangjie` | DataflowInterproc |

Build variants:

```bash
cargo build --release -p atlas-cli                  # all 14 languages
cargo build --release -p atlas-cli --features mcp   # all languages + MCP server
```

## Documentation

Maintained documents:

- [`docs/architecture.md`](docs/architecture.md) — authoritative architecture: constraints, modules, schema, dataflow, capability profiles, design decisions.
- [`docs/requirements.md`](docs/requirements.md) — product scope and acceptance criteria.
- [`docs/roadmap.md`](docs/roadmap.md) — current and future work.
- [`docs/testing.md`](docs/testing.md) — test layers, feature matrix, and release checks.
- [`docs/performance.md`](docs/performance.md) — measured performance baselines.
- [`docs/trace-contract.md`](docs/trace-contract.md) — frozen trace JSON contract and diagnostics model.
- [`docs/domain-rules-language-guide.md`](docs/domain-rules-language-guide.md) — domain-rule registries, matching policy, and language extension rules.
- [`skills/atlas/SKILL.md`](skills/atlas/SKILL.md) — Agent Skill for using Atlas from another agent.

## Development

```bash
# Compile and test the complete workspace surface
cargo check --workspace --all-features
cargo test --workspace --all-features

# Build release binary with MCP
cargo build --release -p atlas-cli --features mcp
```

Conventions:

1. Keep crate dependencies acyclic and aligned with the architecture above.
2. Add or update fixtures when changing extraction, resolution, graph, or trace behavior.
3. Update [`docs/trace-contract.md`](docs/trace-contract.md) and tests when trace response fields or diagnostics change.
4. Update [`docs/architecture.md`](docs/architecture.md) when implemented module boundaries, schema, CLI, MCP, or analysis behavior changes.
5. When changing analysis levels, extraction modes, lazy behavior, capability masks, status, or precision, verify every affected entry path: CLI `index`, shared filesync pipeline, `sync`, lazy structural, lazy dataflow, high-level `Engine`, and raw analysis consumers. See [`docs/testing.md`](docs/testing.md).
6. Keep release-facing documentation in `docs/`; delete obsolete content rather than accumulating stale docs.

## Known limitations

- Atlas performs best-effort semantic analysis, not compiler-grade type checking.
- C/C++ preprocessing is not expanded; include analysis is based on indexed directives and paths.
- Java classpath, Maven, and Gradle resolution are not fully modeled.
- Python dynamic runtime constructs and generated symbols are outside the static extraction model.
- TypeScript barrel/re-export chains use best-effort name fallback rather than a full export graph.
- Dataflow and trace precision varies by language; inspect `atlas doctor` or trace capability metadata before relying on a trace result.
- MCP uses scoped focus extraction for local investigations; run `atlas sync` or `atlas index` outside MCP only when you want a refreshed project-wide cache.
- Call edges (`Calls`, `Instantiates`, `Implements`) are only created when both the caller and callee are indexed project symbols. External library calls (e.g., `useState` from `react`, `printf` from `stdio.h`) do not produce edges. See [Edge visibility](#edge-visibility-project-internal-symbols-only) for details.

## How tree-sitter powers dataflow extraction

Atlas builds its code facts entirely from tree-sitter's Concrete Syntax Tree (CST). Here is the pipeline from raw source to traceable dataflow:

### 1. Parse → CST

```text
source code
  → tree_sitter::Parser (per-language grammar)
  → tree_sitter::Tree (CST)
```

Tree-sitter is an incremental, error-tolerant parser. Atlas uses **14 language grammars** (TypeScript, JavaScript, Python, Java, C, C++, Go, C#, Rust, PHP, Ruby, Kotlin, ArkTS, Cangjie), each compiled from a `grammar.js` into a parser. Parsing is done per-file via a thread-local `Parser` to avoid allocation overhead.

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

### 6. CFG → control flow (12 languages)

```text
CST root (per function)
  → CfgBuilder (walks function body, matching branch/loop/break AST patterns)
  → CfgNode + CfgEdge (Entry → blocks → Exit)
```

CFG construction walks the function AST, identifying control-flow splits and building a graph of basic blocks. Each `CfgNode` records the byte range it covers, and `CfgEdge` connects predecessor → successor. Capability profiles declare limited CFG support for all 14 languages, with explicit per-language limitations. This remains best-effort tree-sitter control flow rather than a compiler CFG; inspect `atlas doctor` for the authoritative capability state.

### Edge visibility: project-internal symbols only

Atlas only creates call edges (`Calls`, `Instantiates`, `Implements`) when **both the caller and the callee are indexed symbols in the project**. If a reference resolves to a symbol outside the project — for example, an import from an external package like `react`, `lodash`, or `std` — no edge is produced.

**How this works**:

1. **Resolution phase** — Each reference is resolved against the project's symbol table. External imports (e.g., `import { useState } from 'react'`) cannot be resolved because the target symbols are not indexed. These references remain unresolved.

2. **Edge building phase** — `GraphBuilder::create_edges_for_reference` verifies that the resolved target symbol exists in the store via `find_symbol_by_id`. If the target symbol is not found (external / not indexed), no incoming edge is added to the project's call graph. Similarly, edges require the **source** symbol (the enclosing function/class containing the reference) to exist — top-level statements without a containing symbol produce no edges.

**Implications**:

| Scenario | Edge created? |
|----------|:---:|
| `foo()` where `foo` is defined in the project | ✅ |
| `foo()` where `foo` is imported from an external package | ❌ |
| `new Foo()` where `Foo` is a class defined in the project | ✅ |
| `useState()` where `useState` comes from `react` | ❌ |
| Top-level expression call (no enclosing function/class) | ❌ |

This design ensures the call graph is **self-contained** — all edges point to symbols that the user can inspect, trace, and navigate within their own codebase. External API calls are intentionally excluded to keep the graph focused on project-internal structure.

### 7. Trace → cross-procedural variable provenance

```text
Symbol graph + DataFlow graph + CFG
  → TraceEngine (backward slice from user-specified location)
  → TracePath (step-by-step provenance: kind, range, file, confidence, evidence)
```

The `TraceEngine` combines symbol-level call graphs with intra-procedural dataflow. At call boundaries, it uses persistent summary tables (`function_summaries`, `summary_param_reaches`, `summary_return_sources`, `summary_call_arg_sources`) with `CrossFunctionBridge` to bridge ArgToParam and ReturnToCall edges across function boundaries without re-extracting dataflow.

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
