# Changelog

All notable changes to Atlas will be documented in this file.

---

## [1.4.0] — 2026-06-04

### Breaking

- Crate versions bumped to 1.4.0.

---

## [1.3.1] — 2026-06-03

### BREAKING: MCP tool refactor (33 → 18 tools)

- **No alias compatibility** — old tool names return "Unknown tool" error
- All tools use clean names without `atlas_` public prefix
- See `docs/architecture.md` §11.3 for the full MCP tool specification

### Tool merges

| Old tools | New tool |
|-----------|----------|
| `open_project`, `status`, `files`, `language_capabilities` | `project(action="open\|status\|files")` |
| `symbol`, `context`, `usages` | `symbol(view="detail\|context\|usages")`, `symbol` parameter |
| `callers`, `callees`, `callgraph`, `neighbors` | `calls(direction="incoming\|outgoing\|both", edge_kinds=[...])` |
| `trace_point`, `trace_variable`, `trace_forward`, `trace_caller_path` | `trace(kind="point\|variable\|forward\|callers")` |
| `dependencies`, `dependents` | `file_dependencies(direction="incoming\|outgoing\|both")`, `file_path` parameter |
| `annotate_fp_dispatch`, `list_fp_annotations`, `delete_fp_annotation` | `fp_dispatches(action="add\|list\|delete")` |
| `atlas_annotate`, `atlas_domain_rules`, `atlas_rule_learn` | `domain_rules(action="add\|list\|delete\|learn")` |
| `jobs`, `atlas_jobs` | `tasks` |
| `atlas_resume` | `resume_task` |
| `atlas_lifecycle` | `lifecycle` |
| `atlas_branch_diff` | `branch_diff` |
| `index`, `search`, `explore`, `path`, `impact`, `task_status`, `wait_for_task` | Unchanged (prefix-only removal) |

### Other changes

- `symbol(view="context")` now outputs structured JSON instead of Markdown
- `file_dependencies` uses `file_path` (no `file_id`)
- `trace(kind="callers\|forward")` `symbol`/`from`/`to` parameters auto-detect hex IDs vs qualified names
- `project(action="status")` always includes language capabilities (no `verbose` gate)

### Branch diff architecture

- `branch_diff` now documents the semantic analysis path as the default
  (`semantic=true`) for MCP callers.
- Semantic branch diff compares `EffectComposition` data instead of only legacy
  single-effect CFG annotations.
- Added structured `BranchDiffIssue` output internally, including asymmetry kind,
  severity, confidence, true/false branch summaries, and evidence-bearing field
  effect details.
- Preserved compatibility with legacy `BranchDiff` consumers by converting
  structured semantic issues back into the existing public result shape.

### Release hardening

- `cargo check --workspace --all-features`, `cargo test --workspace --all-features`,
  strict `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  and `cargo build --release -p atlas-cli --features all-languages,mcp` pass.
- Fixed all-features test compilation by importing `CapabilityMask` in lazy coordinator tests.
- Cleared Clippy release-gate warnings across CLI, MCP, graph, analysis, extraction,
  resolution, search, context, and domain-rules modules.
- Hardened MCP background task tracking against poisoned `Mutex` recovery.
- Removed unused deprecated `serde_yaml` from `atlas-cli` and `Cargo.lock`.
- Added package `repository`, `homepage`, and `documentation` metadata for
  `atlas-cli`, `atlas-engine`, and `atlas-mcp`.
- Updated README, MCP README, architecture, requirements, and Atlas skill docs to
  match the current 18-tool MCP API and DataflowFull language matrix.

---

## [1.3.0] — 2026-06-02

### TUI

- **Interactive TUI**: running `atlas` with no subcommand launches a ratatui-based
  symbol search and detail browser.  Tabbed detail panels (Overview → Callers →
  Callees → Peers → Source) with keyboard navigation.
- TUI auto-indexes on first launch; Ctrl+C during indexing cleanly exits.
- Progress bar shows accurate per-phase throughput via phase-elapsed averaging.

### Domain rules engine

- Language-agnostic `domain_rules` infrastructure: `GenericRuleEngine` with
  `LanguageRuleKinds` trait, `CppOwnershipRules` consumer, and auto-learning
  (`RuleLearningStrategy`).  Rules keyed by deterministic blake3 hash to prevent
  underscore-separated field collisions.

### Lifecycle & analysis

- `analysis/lifecycle.rs`: intra-procedural field-state tracking (Unknown → Assigned →
  Freed → Nullified → Escaped) with rule-backed proof mode.
- `analysis/branch_diff.rs`: sibling-branch side-effect comparison.
- CFG node effect annotation: `cfg_nodes.effect_kind` / `cfg_nodes.target_field`.
- `lifecycle_proof.rs`: `Safe` / `Suspicious` / `Incomplete` verdicts.

### MCP tools

- **Lifecycle**: `atlas_lifecycle(symbol, field)`, `atlas_branch_diff(symbol)`.
- **Domain rules**: `annotate_domain_rule`, `list_domain_rules`, `delete_domain_rule`,
  `approve_domain_rule`.
- **Query resume**: `resume(query_id)` continues a previous partial-result query.
- **AnalysisContract**: `safe_conclusions`, `unsafe_conclusions`, `refinement_jobs`
  in all MCP responses.
- `LazyOrchestrator` + `LazyRefreshQueue` for background graph refresh.
- `CancellationToken` with checkpoints in extraction for interruptible budget enforcement.
- `LazyBudget` with time + file-count constraints.
- Input validation: string length bounds, release-profile hardening.

### Engine

- `atlas-engine` facade exports `ContextView`, `CalleeDetail`, `CallerDetail`,
  `SymbolDef`.
- `CancellationToken` (`CancelCheck` trait): interruptible `extract_file_with_mode_cancellable`
  with CP1–CP6 checkpoints.
- `ClosurePlanner`: import/include-based dependency closure for lazy resolution.
- `ExtractionMode::Manifest` for CLI `--analysis manifest` early-return.
- `CapabilityMask` (u16 bitflags) in `extraction_state`.
- `query_id` atomic counter for resume support.

### Bug fixes

- Fix `domain_rules` ID collision (blake3 + `\xff` delimiter).
- Fix bare `source[start..end]` slice → `source.get(..).unwrap_or("")`.
- Fix `BulkWriteGuard` RAII for safety pragma cleanup on drop/panic.
- Fix `.ok()` silently swallowing DB errors → explicit `QueryReturnedNoRows` match.
- Fix FK guard row-decode failures silently dropped → `eprintln!` warning.
- Fix missing `Resolution` progress phase (`start_phase` before `resolve_all_parallel`).
- Fix `rayon::build_global()` panic when TUI starts after Ctrl+C in CLI index.
- Fix duplicate `layer` column in `find_symbols_by_file` SELECT.
- Fix `GraphSnapshot` doc: clarify `&mut self` write-side mutability.
- Fix broken test compilation and add secret-file patterns to `.atlasignore`.
- Fix worker hang after lazy budget exhaustion.

### Documentation

- Architecture doc: update table count 22→23, add `domain_rules` with language-agnostic description.
- Merge `domain-rules-amendment.md` and `task-lazy-experience.md` into architecture docs.
- Add `domain-rules-language-guide.md`.
- Update MCP tool count 27→28.

### Internal

- Workspace-wide clippy cleanup (`-D warnings` passes).
- Crate versions bumped to 1.3.0.
- MCP skill definition updated.

---

## [1.2.0] — 2026-05-30

### Lazy indexing

- **ResolutionSymbols layer**: lightweight extraction (symbols + imports + scopes, no
  references/dataflow/callsites) for import dependency resolution.
- **ClosurePlanner**: import/include dependency closure computation.
- **Linux augmentation**: `EXPORT_SYMBOL` / `initcall` / `SYSCALL_DEFINE` post-extraction
  enhancement for C.
- **LazyCoordinator**: centralised coordination of lazy structural, resolution_symbols,
  and dataflow extraction jobs with `extraction_jobs` table tracking.
- **Precision tiers**: `Exact` → `PartialExact` → `DegradedStructural` →
  `LocalDataflowOnly` → `ManifestOnly` → `Unavailable`.
- Graph refresh after lazy structural via `replace_files_in_place`.
- `include_roots` coverage for context and trace MCP tools.
- Shared `ensure_structural_for_files` helper across MCP handlers.

### Extraction

- **RecoverySpec** trait: post-extraction recovery for ArkTS structs.
- ArkTS golden fixtures for struct declarations.

### Bug fixes

- Fix doc consistency: `include_roots`, prewarm cache guard, schema comments.
- Fix `PREWARM_RUNNING` flag leak in background prewarm.
- Fix lazy dataflow: extract structural facts before invalidation, wrap in atomic transaction.
- Fix `cfg_nodes` deletion scoped to file; remove broken file-level dataflow guard.
- Fix filesync: three correctness issues from code review.

### MCP

- `atlas_jobs` tool for active extraction job observability.
- Delta graph refresh after lazy structural writes.
- Handler-level regression tests for `include_roots` warnings.

---

## [1.1.0] — 2026-05-28

### Performance

- Resolution: pre-built contexts, lock-free progress via cloned `AtomicU64`, live rate
  display during Phase 1, `sync_channel` streaming Phase 1→2.
- Resolution: pre-computed `lower_names`, `O(1)` import index, `Arc<SymbolDef>` indexes
  (75% fewer heap copies), fuzzy + proximity result caches.
- Graph: preload symbol table in `build_all` — eliminates 315k DB queries.
- DB write: batch size increased 100→500; cleanup batch delete.
- Search: strip quotes in field values, use SQL `LIKE` for non-FTS paths.

### Features

- Multi-language callback detection: `detect_callback_registrations` with generic +
  per-language patterns (Go package prefix, Python decorators).
- `atlas_path`: direction, confidence, breakpoints, production-code preference.
- `atlas_callgraph` with caller/callee summaries.
- `includeCode` parameter for symbol/callgraph/explore tools.
- `atlas_explore` for neighbours grouped by edge kind.
- Function-pointer annotation CRUD: `annotate_fp_dispatch`, `list_fp_annotations`,
  `delete_fp_annotation`.
- AST-driven source extraction with weighted Dijkstra pathfinding.
- Cangjie: `manifest.scm`, CFG support, `@definition.entry` capture.
- Atomic lazy structural re-index and annotation bridging.

### Bug fixes

- Fix C pointer-typed struct fields not extracted; C struct field handling.
- Fix `atlas_path` lazy structural extraction with multi-SymbolId retry.
- Fix `read_symbol_source` return full file content instead of name-only.
- Fix lazy dataflow destroying pre-built full-index dataflow facts.
- Fix `rayon::build_global` idempotency via `Once`.
- Fix resolution: `mutex.lock().unwrap()` → poison-safe.
- Fix graph tests and callers/callees start-node exclusion.
- Fix derived capability profile alignment with static profile.
- Fix TUI: cursor positioning, progress area clearing, completion summary rendering.

### Documentation

- Consolidate architecture docs, align with code.
- Tool counts, MCP schema, FP dispatch annotation references updated.
- Project-internal-only call edge visibility documented.

### Cangjie

- `manifest.scm` for top-level declarations.
- CFG support.
- `@definition.entry` capture for `mainDefinition`.
- Documentation update.

---

## [1.0.0] — 2026-05-25

### First release

Atlas is a local-first semantic knowledge graph engine for LLM agents.  It parses
source code with tree-sitter, stores deterministic code facts in SQLite, and exposes
28 bounded MCP tools plus a CLI for agent-powered codebase navigation.

- 14 languages at DataflowFull capability level.
- 10-stage reference resolution with confidence scoring.
- In-memory graph snapshots with BFS/DFS traversal.
- Cross-function bridging via persisted function summaries (4 tables).
- CLI: `status`, `doctor`, `index`, `sync`, `files`, `mcp`.
- MCP: 28 stdio tools with lazy graph init, background task support, progress
  notifications.
- 14-Cargo-package Rust workspace, edition 2024, SQLite 22-table schema V1.
