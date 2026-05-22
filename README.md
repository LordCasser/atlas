# Atlas

**Local-first semantic knowledge graph builder for codebases.**

Atlas parses your source code with tree-sitter, extracts symbols, scopes, references, calls, callsites, bindings, and dataflow facts, stores them in SQLite, resolves cross-file links, and exposes graph plus trace queries via CLI and MCP (Model Context Protocol) for LLM agents.

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

- **7 MVP languages**: TypeScript, JavaScript, Python, Java, C, C++, ArkTS
- **Deterministic**: tree-sitter AST/query extraction, no AI guessing
- **Local-first**: all data in `.atlas/` per project, no remote services
- **Incremental**: content-hash change detection re-indexes only modified files
- **Rich graph**: symbols, scopes, references, calls, dataflow, imports, container edges
- **Trace mainline**: user-specified variable provenance and caller-path queries for AI analysis
- **MCP native**: graph, search, context, and trace tools for LLM agents
- **CLI**: `init`, `index`, `sync`, `search`, `status`, `doctor`, `trace`

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

Atlas provides a built-in MCP JSON-RPC 2.0 server over stdio, exposing bounded tools for LLM agents:

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
| `atlas_trace_point` | Resolve facts at a code position |
| `atlas_trace_variable` | Trace where a value at a code position comes from |
| `atlas_trace_caller_path` | Trace the farthest caller chain for a target function |
| `atlas_language_capabilities` | Report per-language trace/search/graph capability metadata |

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
| Cangjie ⚠️ | `.cj`, `.cangjie` | `cangjie` | Experimental opt-in, not included in `all-languages` |

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
The current resolver uses a bounded best-effort cascade:
1. **Builtin Filter**: excludes known stdlib names (100+ per language)
2. **Scope-local**: walks lexical scope chain for exact name match
3. **Container-local**: searches enclosing class scope for method references
4. **Same-file**: exact name match within the file
5. **Import/include**: resolves imported symbols and C/C++ local includes where facts exist
6. **Project search fallback**: exact/proximity/fuzzy matching through indexed symbols

Path alias and re-export/barrel resolver components exist, but they are not yet exposed as fully stable project-config-driven resolution in the main path.

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
├── atlas.db                     # SQLite database (schema v1 during rapid development)
└── file_hashes.json             # Sync hash store; index also uses DB content hashes

src/
├── types/                       # Core type system (7 ID types, 11 enums, IR structs)
├── db/                          # SQLite persistence (schema + store)
├── extraction/                  # Tree-sitter parsing + language frontends/adapters
│   ├── queries/                 # Per-language .scm query files
│   └── languages/               # LanguageAdapter implementations
├── resolution/                  # Reference resolution + include/path/export helper components
├── graph/                       # In-memory GraphSnapshot + GraphEngine (BFS/DFS)
├── search/                      # FTS5 + LIKE + fuzzy + camelCase normalization
├── context/                     # AI context builder (callers/callees/peers)
├── sync/                        # Incremental sync (git-aware discovery + hash detection)
├── analysis/                    # Variable provenance and caller-path analysis layer
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

The full verification target is `cargo test --features "all-languages,mcp,sync"`.

---

## Known Limitations

- **Trace capability is language-specific**: CLI and MCP responses include capability metadata; unsupported trace features return partial results with diagnostics.
- **Call arguments**: `callsites.args_json` + call-arg `DataNode` is the single source of truth; the deprecated `callsite_args` table has been removed.
- **Path aliases**: tsconfig path aliases are wired into the resolver; barrel/re-export chains remain name-based (no AST-level re-export graph).
- **ArkTS**: delegates to TypeScript grammar; some ArkTS-specific syntax may not parse
- **Cangjie ⚠️**: experimental opt-in support. It is not part of MVP, default features, or `all-languages`; enable explicitly with `--features cangjie`. Known issues:
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

The active roadmap is in [`docs/05-roadmap.md`](docs/05-roadmap.md). Current priority is:

- Stabilize facts needed for variable provenance and caller-path queries.
- Complete TypeScript/JavaScript/Python Level 3 trace fixtures.
- Keep Java/C/C++/ArkTS capability boundaries explicit in CLI and MCP output; keep Cangjie marked experimental when explicitly enabled.
- Add lightweight function summaries for bounded cross-function provenance.
- Defer crate/workspace splitting until trace E2E behavior is stable.

---

## License

MIT

---

## Contributing

1. Run `cargo test` and `cargo test --features all-languages,mcp,sync` before submitting
2. All new extraction logic should have a corresponding integration test
3. During rapid development, schema changes update the v1 schema and tests; deployment migrations are not a current requirement
4. Language adapters follow the query/normalization `LanguageAdapter` trait in `src/extraction/languages/mod.rs`
