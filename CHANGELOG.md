# Changelog

All notable changes to Atlas will be documented in this file.

---

## [1.4.0] — 2026-06-05

### Breaking

- All 15 workspace crates bumped to 1.4.0.
- `CapabilityMask`: CFG and DATAFLOW bits are now orthogonal. `from_layers("dataflow")`
  no longer sets CFG. `best_capability_name()` returns "summaries" when SUMMARIES bit
  is present.

### Multi-language Semantics (M2–M6)

Atlas now understands resource lifecycle and scope-managed cleanup across **11 languages**
through language-specific domain rule registries and a unified scope-exit analyzer.

- **New domain rule registries** for all 11 DataflowFull languages: Go, Rust, Python,
  TypeScript, Java, C#, Kotlin, Ruby, PHP, C/C++, ArkTS, Cangjie. Each provides
  `alloc_fn`, `free_fn`, `cleanup_fn`, and language-specific `owned_pattern` rules.
- **`CallContext` enum**: language-agnostic call-site context annotations set by the CFG
  builder — `GoGoroutine`, `GoDefer`, `PythonWith`, `JavaTryWith`, `CSharpUsing`,
  `ReactEffectCleanup`, `RubyBlock`, `KotlinUse`.
- **`ScopeExitAnalyzer`**: unified intra-procedural scope-exit analysis. Computes
  `SemanticEffect` chains with Free at block boundaries for Rust `Drop`, C++
  destructors, Python `with`/`__del__`, Java try-with-resources, C# `using`/IDisposable,
  Kotlin `.use`, React `useEffect` cleanup returns, and Go `defer`.
- **New CFG primitives**: `CfgNodeKind::BlockExit` (synthetic block-boundary nodes),
  `DataNodeKind::CleanupReturn` (React effect cleanup return), `EscapeTarget::AsyncContext`
  (goroutine/coroutine escape).
- **`SemanticEffect` enriched**: `consumption_style` (explicit/deferred/context-managed),
  `description` (human-readable origin), `eligible_for_implicit_cleanup` (per-callee
  gating).
- **`OwnershipContract` extended**: `classify_escape()` for goroutine/coroutine escapes,
  `supports_implicit_scope_cleanup()`, `eligible_for_implicit_cleanup()`.
- **CFG support uplift**: C# (was unsupported → 0.72), Kotlin (→ 0.67), Ruby (→ 0.65).
  Cangjie CFG gap closed (was unsupported → 0.60 with body traversal). All languages
  now report CFG status via `supported_with_limitations` with consistent "body traversal
  implemented" annotation.
- **Cangjie** promoted to DataflowFull with CFG, updated FeatureMatrix, and feature-gated
  file extension registration.
- **12 languages** had interprocedural summary confidence floors raised after ArgToParam
  and ReturnToCall bridge verification.

### Pipeline Convergence

Index, sync, and auto-index now share a unified orchestrator pattern.

- **`ProgressSink` trait**: typed progress event abstraction. Each entry point
  (CLI/MCP/TUI) implements its own sink that translates pipeline events into
  native progress displays. `Send + Sync` for rayon safety.
- **`IndexPipeline`**: full index pipeline orchestrator. Replaces duplicated
  phase logic across CLI index, TUI auto-index, and MCP index handlers.
- **`IncrementalPipeline`**: incremental sync pipeline orchestrator. Replaces
  duplicated sync logic across CLI sync and MCP sync handlers.
- **`JobContext`**: unified context for long-running jobs — bundles ProgressSink,
  cancellation token, and optional task ID. Used by CLI, MCP, and TUI.
- **Size reduction**: CLI index 509→274 lines (−46%), TUI auto-index 394→181 lines
  (−54%), SyncEngine 328→145 lines (−56%).
- **Pipeline equivalence tests**: `pipeline_equivalence.rs` verifies that running
  `IndexPipeline` and the CLI's `run_index_pipeline` produces identical DB state
  (files/symbols/edges/summaries) for the same project.

### Capability-Aware Indexing

Dirty-check now respects the requested analysis mode.

- `build_dirty_set_for_mode(store, discovered, root, mode)` replaces `build_dirty_set`.
  A file is clean only when its content hash matches **and** the DB has fresh complete
  file-level `extraction_state` covering the mode's required capability.
- Hash-clean files with insufficient persisted capability are added to the dirty set —
  this enables `atlas index --analysis full` to upgrade a hash-clean manifest/structural
  DB without source changes.
- `file_has_fresh_complete_capability()`: DB-level query checking whether a file at a
  given content hash has all required capability bits.
- `optional()` replaces `map_err(warn).ok()` for metadata key queries — missing
  `last_index_time`/`last_sync_time` is normal empty-DB state, not an error.

### TUI UX

- **Background job system**: `JobManager` executes search and trace on a worker thread.
  `Esc` cancels running jobs. The TUI input remains responsive during long operations.
  Job results delivered via polling on each tick.
- **Instant startup**: TUI no longer blocks on `ensure_index_before_tui`. Starts
  immediately; auto-index runs in background.
- **`SearchSession`**: wraps `Engine` for lazy structural retry on empty manifest
  results, matching MCP `ScopedSearchService` semantics.
- **Command polish**: `truncate_str`, `project_root`, `--analysis` validation, locale
  fixes.

### ScopedSearchService

Shared search engine used by both MCP and TUI.

- **3-tier search**: FTS5 → exact name → LIKE substring fallback.
- **`SearchAnalysis` mode**: `Manifest` (no lazy), `Structural` (always trigger lazy
  on empty), `Auto` (trigger lazy for scopes ≤30 files).
- **Auto skips lazy** when structural data is already present — avoids redundant
  re-extraction.
- **Scope normalization**: strips `./`, `./`, trailing `/`, backslash normalization.
- Returns structured response: results, coverage, triggered_lazy flag, capability mask,
  precision tier, warnings.

### MCP & Storage

- **`storage='auto'`** (default): `open_project` reuses `.atlas/atlas.db` only when the
  DB reports a reusable index (via `read_index_mode`); otherwise opens an in-memory
  zero-footprint session.
- **`ToolCallContext`**: per-tool context with progress sink, cancellation, and task
  tracking. MCP handlers delegate to shared services.
- **Bounded `file_deps`**: scope-limited queries prevent unbounded traversal.
- **MCP router hardening**: `Cell→RwLock` for engine access, `Mutex<Engine>` for
  graph snapshots.

### Impact Analysis Fixes

- **Lazy structural trigger**: `handle_callers`, `handle_callees`, `handle_callgraph`,
  `handle_explore`, `handle_impact` now trigger lazy structural extraction before
  accessing the graph snapshot — fixes empty results after manifest-only index.
- **ArkTS extraction fix**: `@Component` decorator struct detection now uses
  word-boundary-aware scanning instead of `strip_prefix("struct")`.
- **Trace direction control**: `atlas_path` with `direction` parameter fixed for
  reverse provenance queries.

### Bug Fixes & Hardening

- **DB instrumentation**: 20+ previously silent error swallowing sites now properly
  logged via `tracing::warn`/`error`.
- **MCP stability**: blocking event loop (`std::thread::sleep` → `tokio::time::sleep`),
  poisoned `Mutex` recovery, cancellation panic-safety, input validation hardening.
- **Release-blocking P1 fixes**: bounded candidates for impact analysis, cancel wiring
  for async jobs, search delegation through `ScopedSearchService`, prewarm per-store
  guard.
- **Index reliability**: `build_all` deadlock fix, CLI sync `DoneGuard` lifetime fix,
  `FileLock` ownership clarification, prewarm per-store (not global), ripgrep binary
  path fixes, `scope_file_count` semantic fix.
- **Lazy extraction**: BFS dedup, interleaved budget check, string constants fix,
  `has_cfg` propagation, worker hang after budget exhaustion, lazy callsite remapping.
- **CFG fixtures**: `cfg_if_else` + `cfg_loop` golden fixtures for 11 languages,
  `with_lifecycle` for Python, `goroutine` for Go, `try_resource` for Java,
  `use_resource` for Kotlin, `using_dispose` for C#, `procedural_resource` for PHP,
  `scope_exit` for Rust.
- **Type system alignment**: `FeatureMatrix.cfg` and `supported_features` list now
  asserted consistent via compile-time tests. `CapabilityMask` layer → bit mapping
  verified. `CfgNode.call_context` properly serialized/deserialized.

### Documentation

- Architecture docs: updated module boundaries (ProgressSink, JobContext,
  ScopedSearchService, pipeline orchestrators), capability profiles, database schema,
  tool references.
- Requirements: re-index mode-awareness rules, optional metadata semantics.
- Temporary language evolution documents deleted — content folded into core docs.
- README and MCP skill definition synced with 18-tool API.

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
