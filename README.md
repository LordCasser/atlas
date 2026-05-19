# Atlas

**Local-first semantic knowledge graph builder for codebases.**

Atlas parses your source code with tree-sitter, extracts symbols, scopes, references, calls, and dataflow edges, stores them in SQLite, resolves cross-file links, and exposes the knowledge graph via CLI and MCP (Model Context Protocol) for LLM agents.

```
Source Code                 Atlas Engine                  LLM Agent
  │                             │                             │
  ├─ *.ts, *.py, *.java  ──→  tree-sitter extract  ──→  SQLite DB
  ├─ *.c, *.cpp, *.ets         resolve references           │
  └─ ...                       build graph               MCP Server
                                                        (JSON-RPC)
```

---

## Features

- **8 languages**: TypeScript, JavaScript, Python, Java, C, C++, ArkTS, Cangjie
- **Deterministic**: tree-sitter AST/query extraction, no AI guessing
- **Local-first**: all data in `.atlas/` per project, no remote services
- **Incremental**: content-hash change detection re-indexes only modified files
- **Rich graph**: symbols, scopes, references, calls, dataflow, imports, container edges
- **MCP native**: 12 tools for LLM agents — search, callers, callees, call graph, path finding, impact analysis, context
- **CLI**: `init`, `index`, `sync`, `search`, `status`, `doctor`

---

## Quick Start

### Prerequisites

- **Rust** 1.85+ (edition 2024)
- **Git** (for `git ls-files` file discovery; graceful fallback to filesystem walk)

### Install

```bash
git clone <repo-url>
cd atlas
cargo build --release --features all-languages,cli
```

The binary is at `./target/release/atlas`.

### First Project

```bash
# Initialize Atlas in your project
atlas init --project /path/to/your/project

# Index the codebase
atlas index --project /path/to/your/project

# Check status
atlas status --project /path/to/your/project

# Search for a symbol
atlas search "UserService" --project /path/to/your/project
```

---

## CLI Commands

| Command | Description |
|---------|-------------|
| `atlas init` | Create `.atlas/` directory with SQLite database |
| `atlas index` | Discover and index all source files in a project |
| `atlas sync` | Incremental sync (re-indexes only changed files) |
| `atlas search <query>` | Search symbols by name (FTS5 + LIKE + fuzzy cascade) |
| `atlas status` | Show file/symbol/edge counts and DB stats |
| `atlas doctor` | Check environment: SQLite FTS5, grammar support, schema |

All commands accept `-p / --project <PATH>` to specify the project root (defaults to `.`).

### Search Examples

```bash
# Basic search
atlas search "calculate" --project .

# Limit results
atlas search "User" --limit 20

# Search does substring matching (LIKE fallback) + camelCase normalization
# "getUser" matches "get_user" and vice versa
```

---

## MCP Server

Atlas provides a built-in MCP JSON-RPC 2.0 server over stdio, exposing 12 tools for LLM agents:

| Tool | Description |
|------|-------------|
| `atlas_status` | Project overview: file/symbol/edge counts |
| `atlas_files` | List all indexed files with language and status |
| `atlas_search` | Search symbols by name (FTS5 + fuzzy, kind filter) |
| `atlas_symbol` | Get detailed info for a specific symbol |
| `atlas_neighbors` | Get incoming/outgoing edges for a symbol |
| `atlas_callers` | List functions that call a given function |
| `atlas_callees` | List functions called by a given function |
| `atlas_callgraph` | BFS call graph from a symbol, configurable depth |
| `atlas_path` | Shortest path between two symbols in the graph |
| `atlas_explore` | Symbol details + all neighbor edges with kinds |
| `atlas_impact` | Impact analysis: what depends on this symbol? |
| `atlas_context` | AI context: callers + callees + peers as markdown |

### Configuring MCP (Claude Desktop / Cursor)

Add to your MCP client configuration (`mcp.json` or `claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "atlas": {
      "command": "/path/to/target/release/atlas",
      "args": ["mcp", "--project", "/path/to/your/project"]
    }
  }
}
```

The `mcp` subcommand starts a persistent MCP server process that the LLM agent queries on demand. You must run `atlas index` before starting the MCP server.

---

## Supported Languages

| Language | Extensions | Feature Flag | Grammar |
|----------|-----------|-------------|---------|
| TypeScript | `.ts`, `.tsx` | `typescript` | tree-sitter-typescript |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | `javascript` | tree-sitter-typescript (shared) |
| Python | `.py`, `.pyi`, `.pyx` | `python` | tree-sitter-python |
| Java | `.java` | `java` | tree-sitter-java |
| C | `.c`, `.h` | `c` | tree-sitter-c |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx` | `cpp` | tree-sitter-cpp |
| ArkTS | `.ets` | `arkts` | tree-sitter-typescript (delegated) |
| Cangjie ⚠️ | `.cj` | `cangjie` | tree-sitter-cangjie (git dep) — see [Limitations](#limitations) |

Default features: `typescript`, `javascript`, `python`, `cli`. Enable additional languages with `--features`:

```bash
cargo build --release --features "all-languages,cli"
```

---

## How It Works

### 1. Extraction
Each source file is parsed with tree-sitter. Per-language `.scm` queries capture:
- **Definitions**: functions, classes, methods, variables, enums, macros
- **References**: calls, type references, field accesses, instantiations
- **Imports**: import/include/using statements with module resolution
- **Scopes**: lexical scopes (file, class, function, block, namespace)
- **Dataflow**: parameters, returns, assignments, field reads/writes

### 2. Post-Extraction
- **Scope Tree**: stack-based nesting assigns `parent_id` to scopes and `scope_id` to symbols
- **Container Assignment**: class-like scopes are assigned as container for member symbols
- **Callsite Derivation**: `Call` references with source symbols generate callsites

### 3. Resolution
A 6-strategy cascade resolves cross-file references:
1. **Builtin Filter**: excludes known stdlib names (100+ per language)
2. **Scope-local**: walks lexical scope chain for exact name match
3. **Container-local**: searches enclosing class scope for method references
4. **Same-file**: exact name match within the file
5. **Import**: resolves imported symbols from cross-file modules
6. **FTS5 Search**: project-wide fuzzy search as fallback

### 4. Graph
Resolved references create structural edges (`Calls`, `Instantiates`, `Implements`, `References`, `Contains`). The full graph is loaded in-memory for querying (`callers`, `callees`, `callgraph`, `shortest_path`, `impact`).

### 5. Search
A 3-stage cascade:
1. **FTS5 prefix**: primary match, fastest
2. **LIKE fallback**: substring match on name/qualified_name
3. **Fuzzy prefix**: 3-char FTS5 prefix + post-filter

Name matching uses 6-tier similarity: exact → case-insensitive → camelCase/snake_case normalized → word overlap → Levenshtein.

---

## Project Structure

```
.atlas/                          # Per-project Atlas state
├── atlas.db                     # SQLite database (schema v3)
└── file_hashes.json             # Incremental sync hash store

src/
├── types/                       # Core type system (7 ID types, 11 enums, IR structs)
├── db/                          # SQLite persistence (schema + store)
├── extraction/                  # Tree-sitter parsing + 7 language adapters
│   ├── queries/                 # Per-language .scm query files (6 × 5 = 30 files)
│   └── languages/               # LanguageAdapter implementations
├── resolution/                  # 6-strategy reference resolution
├── graph/                       # In-memory GraphSnapshot + GraphEngine (BFS/DFS)
├── search/                      # FTS5 + LIKE + fuzzy + camelCase normalization
├── context/                     # AI context builder (callers/callees/peers)
├── sync/                        # Incremental sync (git-aware discovery + hash detection)
├── mcp/                         # MCP JSON-RPC 2.0 server (feature-gated)
├── cli/                         # Clap CLI commands (feature-gated)
└── lib.rs                       # Module declarations
```

---

## Architecture Principles

- **Separation of Concerns**: Each module has a single, well-defined responsibility
- **Deterministic IDs**: All IDs are blake3 hashes, enabling idempotent indexing
- **Deref Coercion**: `Store` derefs to `StoreReader` for clean read/write separation
- **Feature Gating**: Language adapters, MCP server, CLI optional via Cargo features
- **Best-Effort Resolution**: Resolution errors surface as warnings, don't block the pipeline

---

## Testing

```bash
# Default test suite (TS/JS/Python languages + CLI)
cargo test

# Full test suite (all languages + MCP + sync)
cargo test --features "all-languages,mcp,sync"

# Integration tests only
cargo test --test integration

# Run with release build
cargo build --release --features all-languages,cli
./target/release/atlas index --project /tmp/test-project
```

Test coverage:
- **144 unit tests** (default features) — all pass
- **176 unit tests** (full features) — all pass
- **9 integration tests** — cross-file resolution, graph queries, scope tree, language detection
- **0 build warnings**

---

## Known Limitations

- **ArkTS**: delegates to TypeScript grammar; some ArkTS-specific syntax may not parse
- **Cangjie ⚠️**: three-level issue prevents indexing despite functional adapter code:
  1. tree-sitter-cangjie grammar ABI 15 pre-dates Atlas tree-sitter 0.24.7 (max ABI 14)
  2. grammar.js has been rewritten (using modern node types like `functionDefinition`, `classDefinition`, `postfixExpression`) but parser.c was stale — regeneration with tree-sitter CLI 0.24.7 resolved ABI + naming but revealed:
  3. Atlas `.scm` queries reference node types (e.g. `typeAnnotation`) that don't exist in the current grammar, requiring query-level fixes
  - **Status**: removed from `all-languages` feature (opt-in only via `--features cangjie`). Full fix requires `.scm` query updates to match grammar node types.
- **C/C++**: no preprocessor expansion; only `#include` directives are parsed for imports
- **Java**: no classpath/Maven/Gradle resolution; cross-file resolution is name-based
- **Python**: no dynamic type inference; runtime-constructed symbols are not captured
- **Performance**: full in-memory graph for 100k+ symbol projects (~50MB memory budget)
- **Concurrency**: Store uses single `Mutex<Connection>`; MCP server is single-threaded

---

## Roadmap

- [ ] C/C++ preprocessor expansion for accurate include resolution
- [ ] Framework-specific resolvers (React, Django, Spring)
- [ ] Taint analysis pipeline (dataflow edge tracking)
- [ ] Parallel indexing with rayon
- [ ] Support for Rust, Go, C#, Ruby, Swift, Kotlin, PHP
- [ ] Fix Cangjie `.scm` queries to match tree-sitter-cangjie grammar node types
- [ ] MCP Server concurrency with connection pool

---

## License

MIT

---

## Contributing

1. Run `cargo test` and `cargo test --features all-languages,mcp,sync` before submitting
2. All new extraction logic should have a corresponding integration test
3. Schema changes require bumping `CURRENT_SCHEMA_VERSION` and adding migration SQL
4. Language adapters follow the `LanguageAdapter` trait in `src/extraction/languages/mod.rs`
