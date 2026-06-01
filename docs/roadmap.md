# Atlas Roadmap

This roadmap tracks **current and future work only**.

## 1. Current release focus: V1 release

Goal: ship a stable first version where CLI and MCP tools are usable by end users and agents against a local repository.

### 1.1 Packaging and installation

- Publish or document a repeatable release build flow for macOS, Linux, and Windows.
- Document verified platform matrix, minimum Rust version, and feature choices.
- Decide whether V1 is distributed as source-only, release binaries, or both.
- Add release notes / changelog entry for the first public version.

### 1.2 User-facing documentation

- Keep `README.md` as the primary user entry point: installation, quickstart, CLI, MCP, architecture, language support, limitations.
- Keep `docs/trace-contract.md` as the stable reference for trace JSON output.
- Keep `docs/architecture.md` as the single authoritative architecture document.

### 1.3 MCP production hardening

- Freeze V1 MCP tool naming: short names without `atlas_` prefix. ✅ Done.
- Freeze V1 tool schemas and document argument requirements.
- Add machine-readable version metadata for MCP clients.
- Finalize graph snapshot refresh semantics.
- Keep all MCP outputs bounded and ensure truncation is visible in the response.

### 1.4 CLI and database release gates

- Ensure `atlas doctor` exposes release-relevant state.
- Compatibility: V1 schema with no migration chain (direct DDL changes).
- Make `.atlas/` cleanup and rebuild guidance explicit.
- Keep JSON output stable for scripted use.
- Publish verified performance baselines.

### 1.5 Release smoke tests

```bash
cargo test
cargo test -p atlas-cli --features all-languages
cargo test -p atlas-cli --features mcp
cargo test -p atlas-cli --features "all-languages,mcp"
```

## 2. Completed work

### 2.1 DataflowFull + persistent summary layer ✅

All 14 languages are now at `DataflowFull` level. The current schema added 4 persistent summary tables (`function_summaries`, `summary_param_reaches`, `summary_return_sources`, `summary_call_arg_sources`) with `CrossFunctionBridge` for ArgToParam/ReturnToCall interprocedural bridges.

> **Known gap**: CFG builder (`cfg_builder.rs`) has placeholder `walk_if`/`walk_loop` implementations — if/else sub-blocks and loop bodies are not traversed. This means CFG output is structurally incomplete for conditional and loop branches. The `DataflowFull` label reflects the dataflow/lexical/reference pipeline, not CFG completeness.

### 2.2 Lazy Index (three phases) ✅

- **P0: Scope Index** — `--include`/`--scope`/`--exclude` range-limited indexing.
- **P1: Manifest Extraction** — `ExtractionMode::Manifest` for lightweight top-level symbol extraction.
- **P2: Lazy Structural** — on-demand structural extraction via `LazyStructuralService`.

### 2.3 Workspace/crate split ✅

Project split into `atlas-engine` facade, engine internal crates, `atlas-mcp`, and `atlas-cli`.

### 2.4 Performance optimizations ✅

P0-P7 optimizations completed: PhaseTimings, hash-based dirty-set, thread-local parsers, batch DB writes, GlobalSymbolIndex, Rayon parallel edges, on-demand dataflow/CFG, language-capability-driven skip.

### 2.5 MCP tool consolidation ✅

All tools use short names (no `atlas_` prefix). 27 tools registered.

> **Next step (post-V1)**: namespace-style merge 27 → 16, see `8.1`. V1 freezes the 27-tool surface.

## 3. Trace and language capability work

### 3.1 Capability alignment

- Keep `language_capabilities` and `atlas doctor` aligned with actual compiled features.
- For each language, maintain explicit limitations and confidence floors.
- Ensure unsupported or partial trace queries return diagnostics rather than silent empty results.

### 3.2 Path-level validation

Continue expanding end-to-end smoke tests for all languages.

## 4. Graph and performance evolution

### 4.1 Graph/dataflow/CFG loading

- Keep symbol graph snapshots as the main graph-query accelerator.
- Load dataflow and CFG facts by file/function/slice when trace queries need them.
- Avoid unbounded in-memory loading for fine-grained dataflow and CFG facts.

### 4.2 Performance targets

- Keep `docs/performance.md` updated with release baselines.
- Track index time, DB size, memory use, and MCP query latency.
- Prioritize resolution and DB write bottlenecks.

## 5. Public API stabilization

`atlas-engine` already exists as a facade crate. The next step is API stabilization:

- Define the minimal supported public API surface.
- Avoid leaking internal schema details unnecessarily.
- Document feature flags and language availability.
- Keep CLI/MCP as consumers of the same engine behavior.
- Lock trace response contracts before promising downstream compatibility.

## 6. Future product lines

### 6.1 Atlas mainline

- Indexing and incremental sync.
- Symbol graph and dependency graph.
- Search/context/impact analysis.
- Variable provenance and caller-path tracing.
- MCP-driven agent context.

### 6.2 Corpus (not in V1)

A multi-version source corpus system for Linux/U-Boot/BusyBox-style repositories remains a separate future product line:

```text
Atlas:  project-relative path + local workspace DB
Corpus: git blob + version/tag/path mappings
```

## 7. Not planned for V1

- Full compiler-grade type checking.
- Full C/C++ preprocessing, template instantiation, overload resolution, or alias analysis.
- Full Python dynamic/runtime symbol resolution.
- Java Maven/Gradle/classpath completeness.
- Automatic vulnerability scanning, taint rules, finding generation, or SAST product features.
- Multi-version source corpus indexing.

## 8. Post-V1 simplification backlog

Deferred from V1; pick up in v1.2 / v2.0 once V1's MCP surface is frozen and downstream clients have anchored on it.

### 8.1 MCP tool surface consolidation (27 → 16)

The V1 27-tool surface has 4 clear namespace-style merge opportunities that reduce the description footprint by ~41% and save ~220 LOC of handler boilerplate. Implementation strategy: **Deprecate + Replace** — add 4 new namespace tools, keep the 16 old tool names as deprecated aliases routing to the new handlers, remove aliases by v2.0.

| Group | Tools merged | New name | Dispatch parameter | Aliases |
|---|---|---|---|---|
| A: graph (7) | `neighbors`, `callers`, `callees`, `callgraph`, `path`, `explore`, `impact` | `graph` | `action: enum` | 7 |
| B: trace (4) | `trace_point`, `trace_variable`, `trace_caller_path`, `trace_forward` | `trace` | `kind: enum` | 4 |
| C: file_deps (2) | `dependencies`, `dependents` | `file_deps` | `direction: enum` | 2 |
| D: fp_annotations (3) | `annotate_fp_dispatch`, `list_fp_annotations`, `delete_fp_annotation` | `fp_annotations` | `action: enum` | 3 |

**Why not in V1**: V1 freezes MCP tool schemas for downstream client stability. Introducing 4 new tools + 16 aliases in V1 would expand the deprecation surface during the stabilization window.

**Why not pure breaking change (27 → 16 in one release)**: breaks every MCP client already anchoring on the V1 names. Alias-based deprecate+replace keeps V1 contracts valid until v2.0.

**Alias routing** (`crates/atlas-mcp/src/tools/mod.rs::call_tool`):
```rust
"neighbors"  => self.handle_graph(&with_action(args, "neighbors")),
// ... 7 graph alias
"dependencies" => self.handle_file_deps(&with_direction(args, "outgoing")),
"dependents"   => self.handle_file_deps(&with_direction(args, "incoming")),
"trace_point"  => self.handle_trace(&with_kind(args, "point")),
// ... 3 trace alias
"annotate_fp_dispatch" => self.handle_fp_annotations(&with_action(args, "add")),
// ... 2 fp_annotations alias
```
Three ~5-line helpers (`with_action`, `with_kind`, `with_direction`) inject the dispatch string into `args` and forward to the new handler.

**Boilerplate savings inside the merged groups**:
- `tools/trace.rs` (397 LOC): `include_roots` resolution + lazy_structural warning injection is repeated in all 4 handlers → extract `resolve_trace_endpoint()` + `finalize_trace_response()` (≈ -150 LOC).
- `tools/dependencies.rs` (42) and `dependents.rs` (41): near-mirror, only differ in the store call → merge into one new `tools/file_deps.rs` (~50 LOC).

**Not merged (kept as-is)**: `index`, `open_project` (entry semantics + progress state machine), `search` (name lookup ≠ graph traversal), `symbol` (name resolution + lazy structural fallback), `context` (markdown rich response), `status`, `files`, `language_capabilities` (read-only metadata), `usages` (full reference set, not just caller path), `task_status`, `wait_for_task` (poll vs block semantics).

**File changes**:
- `tools/mod.rs`: 1554 → ~1450 LOC (remove 16 `make_all_tools` entries).
- `tools/trace.rs`: 397 → ~250 LOC.
- `tools/dependencies.rs` + `tools/dependents.rs`: deleted.
- `tools/graph.rs` (854) / `tools/annotations.rs` (318): keep existing handlers, privatize, add top-level dispatch.
- **New** `tools/file_deps.rs` (~50 LOC).

**Estimated effort**: 1.5-2 working days.

**Recommended order**:
1. Group C `file_deps` (30 min) — establish the alias pattern.
2. Group B `trace` (1-2 hr) — highest ROI; biggest boilerplate elimination.
3. Group D `fp_annotations` (30 min).
4. Group A `graph` (3-4 hr) — heaviest logic; ensure alias parity for `path` / `callgraph` / `explore`.
5. One equivalence integration test across 16 old names + 4 new names.
6. `docs/architecture.md:33` (27 → 16) and `CHANGELOG.md` update; v1.2 release notes announce the 4 merges, 16 aliases, and v2.0 removal.

**Verification**:
- Each deprecated alias returns **byte-identical** JSON to the original tool.
- 5 existing regression tests (e.g. `trace_point_invalid_include_roots_returns_diagnostics`) still pass — they call internal handlers directly, not through the MCP protocol.
- New equivalence integration test: 16 old names + 4 new names produce equal bodies.
- `docs/architecture.md:33` updated from 27 → 16; `CHANGELOG.md` records the merge, aliases, and removal timeline.

**Open questions to resolve at v1.2 kickoff**:
1. Deprecation window length — recommended: until v2.0 (~6-12 months).
2. Whether to take a one-shot hard cut for `fp_annotations` (internal-only surface, deprecate+replace may be unnecessary).
3. Whether to split graph into read-only (`neighbors`/`callers`/`callees`) vs. traversal (`callgraph`/`path`/`explore`/`impact`) — current recommendation: keep all 7 actions in one tool (clients handle enum dispatch well).
4. Whether to refactor `mod.rs::call_tool`'s large match into a `HashMap` dispatch in the same pass — **not recommended** (explicit match is friendlier to IDEs and code review).
