# Atlas Roadmap

This roadmap tracks **current and future work only**. Completed milestones, historical plans, and superseded implementation notes are archived under [`docs/archive/`](./archive/).

## 1. Current release focus: V1 basic usability

Goal: ship a first version where the CLI and MCP tools are basically usable by end users and agents against a local repository.

### 1.1 Packaging and installation

- Publish or document a repeatable release build flow for macOS, Linux, and Windows.
- Document verified platform matrix, minimum Rust version, and feature choices.
- Decide whether V1 is distributed as source-only, release binaries, or both.
- Add release notes / changelog entry for the first public version.

### 1.2 User-facing documentation

- Keep `README.md` as the primary user entry point: installation, quickstart, CLI, MCP, architecture, language support, limitations.
- Keep `docs/trace-contract.md` as the stable reference for trace JSON output.
- Add or maintain troubleshooting notes for:
  - missing `.atlas/atlas.db`
  - stale indexes after source changes
  - schema too old / too new
  - feature not compiled for a language or MCP
  - path outside project root
  - bounded or truncated MCP output

### 1.3 MCP production hardening

- Freeze V1 MCP tool naming:
  - Atlas-specific tools use the `atlas_` prefix.
  - Generic semantic tools remain `usages`, `dependencies`, and `dependents`.
- Freeze V1 tool schemas and document argument requirements.
- Add machine-readable version metadata for MCP clients:
  - Atlas version
  - schema version
  - tool contract version
  - compiled Cargo features
- Finalize graph snapshot refresh semantics:
  - whether graph-backed tools auto-refresh after external `atlas sync`
  - whether users must restart MCP
  - which status/diagnostic output exposes staleness
- Keep all MCP outputs bounded and ensure truncation is visible in the response.

### 1.4 CLI and database release gates

- Ensure `atlas doctor` exposes release-relevant state:
  - schema status
  - compiled features
  - language capability profiles
  - SQLite / FTS readiness
  - whether the project has been indexed
- Decide the V1 database compatibility policy:
  - fresh V1 DB only, or
  - forward migration promise starting at V1.
- Make `.atlas/` cleanup and rebuild guidance explicit.
- Keep JSON output stable for scripted use.

### 1.5 Release smoke tests

Maintain a release smoke checklist that covers:

```bash
cargo test
cargo test -p atlas-cli --features all-languages
cargo test -p atlas-cli --features mcp
cargo test -p atlas-cli --features "all-languages,mcp"
```

And binary-level smoke against a sample project:

```bash
atlas init --project <sample>
atlas index --project <sample>
atlas status --project <sample>
atlas search "<known-symbol>" --project <sample>
atlas trace point --project <sample> --file <file> --line <line> --column <column> --json
atlas mcp --project <sample>
```

## 2. Trace and language capability work

Atlas already has local dataflow and trace paths for multiple languages, but V1 documentation must not imply compiler-grade or complete cross-function analysis.

### 2.1 Capability alignment

- Keep `atlas_language_capabilities` and `atlas doctor` aligned with actual compiled features.
- For each language, maintain explicit limitations and confidence floors.
- Ensure unsupported or partial trace queries return diagnostics rather than silent empty results.

### 2.2 Path-level validation

For every `DataflowBasic` language, keep or add at least one end-to-end smoke test:

```text
real source -> index -> trace query -> path steps/range/confidence/provenance assertions
```

Priority languages for stricter assertions:

1. TypeScript
2. JavaScript
3. Python
4. Java / C / C++ / ArkTS
5. Go / Rust / C# / PHP / Ruby / Kotlin

### 2.3 Lightweight cross-function trace

Continue evolving the existing query-time summary work, but document it as bounded and best-effort until fixtures prove precision:

- caller argument -> callee parameter bridge
- callee return -> caller call result bridge
- recursion / cycle guard
- max-depth and output-budget truncation
- confidence decay across summary edges

## 3. Graph and performance evolution

### 3.1 Graph/dataflow/CFG loading

Current direction:

- Keep symbol graph snapshots as the main graph-query accelerator.
- Load dataflow and CFG facts by file/function/slice when trace queries need them.
- Avoid unbounded in-memory loading for fine-grained dataflow and CFG facts.

Future API shape:

```text
GraphSnapshot       -> symbol-level graph
DataflowReader      -> bounded dataflow traversal
CfgReader           -> function/file-local CFG traversal
TraceEngine         -> composes symbol graph + local readers + summaries
```

### 3.2 Performance targets

- Keep `docs/08-performance-baseline.md` updated with release baselines.
- Track index time, DB size, memory use, and MCP query latency on small/medium/large repositories.
- Prioritize resolution and DB write bottlenecks before adding expensive new analysis passes.

## 4. Public API stabilization

`atlas-engine` already exists as a facade crate. The next step is API stabilization, not another major workspace split.

Before calling the engine API stable:

- define the minimal supported public API surface
- avoid leaking internal schema details unnecessarily
- document feature flags and language availability
- keep CLI/MCP as consumers of the same engine behavior
- lock trace response contracts before promising downstream compatibility

## 5. Future product lines

### 5.1 Atlas mainline

Continue focusing on local, single-repository, single-version indexing:

- indexing and incremental sync
- symbol graph and dependency graph
- search/context/impact analysis
- variable provenance and caller-path tracing
- MCP-driven agent context

### 5.2 Corpus is out of scope for V1

A multi-version source corpus system for Linux/U-Boot/BusyBox-style repositories remains a separate future product line. It should not be merged into Atlas V1 because it needs a different identity model:

```text
Atlas:  project-relative path + local workspace DB
Corpus: git blob + version/tag/path mappings
```

Do not add Corpus-specific schema or roadmap items to Atlas unless the engine API has stabilized and the work is explicitly scoped as a separate crate/application.

## 6. Not planned for V1

- Full compiler-grade type checking.
- Full C/C++ preprocessing, template instantiation, overload resolution, or alias analysis.
- Full Python dynamic/runtime symbol resolution.
- Java Maven/Gradle/classpath completeness.
- Automatic vulnerability scanning, taint rules, finding generation, or SAST product features.
- Multi-version source corpus indexing.
