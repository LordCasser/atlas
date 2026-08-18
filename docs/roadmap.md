# Atlas Roadmap

Tracks **goals and remaining work**. Landed capabilities are stated in the present tense.  
Version-to-version changes belong only in [`CHANGELOG.md`](../CHANGELOG.md).

## 1. Current development focus: post-Atlas 1.6.1

Workspace version is **1.6.1**. Everything after git tag **`v1.6.0`** is an
latest released baseline is an indexing performance release: per-phase bulk-load index staging, predicate
pushdown in function-pointer and summary queries, removal of the strategy-6
scope mutex, directory-first proximity fuzzy search, a pooled SQLite read path,
and sliding-window progress rates. No MCP tool name, schema, trace envelope,
SQLite schema version, or extraction semantic changed; index contents are
identical to 1.6.0. Post-1.6.1 development resumes language-precision work;
unreleased semantic changes are tracked in `CHANGELOG.md`'s Unreleased section.
Tag **`v1.6.0`** remains sealed; see `CHANGELOG.md` §1.6.1.

Tag **`v1.5.5`** and earlier remain sealed. The CFG v3 milestone (structured
control-transfer/exception/resource facts, Schema V3 persistence, aligned
Focus/Index/MCP consumers, cross-language real-project regression coverage)
shipped in 1.6.0; see `CHANGELOG.md` §1.6.0.

Goal: ship a stable first version where CLI and MCP tools are usable by end users and agents against a local repository.

### 1.1 Packaging and installation

- Publish or document a repeatable release build flow for macOS, Linux, and Windows.
  ✅ Done: README documents the local release build command and the GitHub
  release workflow builds the same `atlas-cli --features mcp` binary for the
  release targets.
- Document verified platform matrix, minimum Rust version, and feature choices.
  ✅ Done: README lists release assets for Linux x86_64/arm64/riscv64, macOS
  arm64, Windows x86_64/arm64, Rust 1.85+, and the `mcp` release feature.
- Decide whether releases are distributed as source-only, release binaries, or both.
  ✅ Done: README states releases are source plus binaries.
- Add release notes / changelog entry for the current public version. ✅ Done:
  `CHANGELOG.md` contains a dedicated 1.6.1 indexing-performance section above
  the sealed 1.6.0 CFG v3 milestone and the sealed 1.5.x history.

### 1.2 User-facing documentation

- Keep `README.md` as the primary user entry point: installation, quickstart, CLI, MCP, architecture, language support, limitations. ✅ Done.
- Keep `docs/trace-contract.md` as the stable reference for trace JSON output. ✅ Done.
- Keep `docs/architecture.md` as the single authoritative architecture document. ✅ Done.

### 1.3 MCP production hardening

- Freeze V1 MCP tool naming: short names without `atlas_` prefix. ✅ Done.
- Freeze V1 tool schemas and document argument requirements. ✅ Done: tool
  names, schema property sets, and required fields are locked by
  `schema_validation`; README documents primary required arguments.
- Add machine-readable version metadata for MCP clients. ✅ Done: `project(status)`
  returns `server.atlas_version`, `server.tool_contract_version`, and
  `server.compiled_features`, with regression coverage.
- Finalize graph snapshot refresh semantics. ✅ Done: lazy writes enter
  `record_lazy_writes()`, graph-backed requests flush `maybe_refresh_graph()`,
  handler-local structural writes can call `force_refresh_graph()`, cumulative
  writes schedule deferred full rebuild, and generation changes force visible
  refresh; regression tests cover empty batches, external writes, preservation
  across refresh, queue deduplication, and rebuild threshold behavior.
- Keep all MCP outputs bounded and ensure truncation is visible in the response.
  ✅ Done: individual handlers bound result collections/source and expose
  structured truncation metadata (`project(files)` defaults to 500 rows).
  `ToolRouter::call_tool()` returns one complete JSON content block and never
  byte-slices a serialized response; oversized-response regression coverage
  parses the result and verifies the truncation fields.

### 1.4 CLI and database release gates

- Ensure `atlas doctor` exposes release-relevant state. ✅ Done: doctor
  prints Atlas version, Schema V4 state, canonical index mode from `Store`, compiled
  features, and per-language capability profiles; helper tests cover schema and
  index-mode reads.
- Compatibility: Schema V4 with no migration chain for older development schemas
  (direct DDL changes + re-index). ✅ Done: `init_schema()` only initializes
  empty unversioned databases or current-version databases, stamps fresh DBs
  with `CURRENT_SCHEMA_VERSION`, and rejects non-empty v0 development databases
  with rebuild guidance.
- Make `.atlas/` cleanup and rebuild guidance explicit. ✅ Done: failing
  database/schema/index-mode checks print explicit `atlas index --project ...`
  rebuild guidance and `.atlas/atlas.db` cleanup instructions for incompatible
  development databases.
- Keep MCP and trace JSON output stable for scripted/agent use; CLI stdout JSON
  is not part of the current 1.6.x command surface. ✅ Done: engine trace
  envelope tests lock the serialized V1 fields, MCP schema validation freezes
  tool argument shapes, and `handler_regression` covers the `trace` tool through
  `ToolRouter::call_tool()` including `query_id`/`analysis` and retired-field
  exclusions.
- Publish verified performance baselines. ✅ Done: `docs/performance.md`
  includes the 2026-07-08 release-mode Atlas self-index smoke baseline on a
  clean `git archive HEAD` checkout, plus historical large-project baselines.

### Release smoke tests

```bash
cargo fmt --all -- --check
cargo check --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --release -p atlas-cli --features mcp
```

✅ Done for 1.5.4: verified on 2026-07-13. Formatting, all-feature workspace
check/tests, and the release MCP binary build completed with exit code 0;
`target/release/atlas --version` reports 1.5.4 and `atlas doctor` passes for
both the Atlas checkout and `examples/arkts_example`. Residual risks: the
checked-in `examples/arkts_example/.atlas` DB predates the final fix and must
be removed (`.atlas/atlas.db`) and re-indexed, since `doctor` cannot detect
this via unchanged source hashes; local verification was macOS arm64 only —
Linux and Windows coverage is via the gated release matrix.

✅ Done for 1.5.5: verified on 2026-07-14. Formatting, all-feature workspace
check/tests, and the release MCP binary build completed with exit code 0;
`target/release/atlas --version` reports 1.5.5 and `atlas doctor` passes for the
Atlas checkout. Local verification was macOS arm64; Linux and Windows coverage
remains the responsibility of the gated release matrix.

✅ Done for 1.6.0: verified on 2026-07-22. Formatting, all-feature workspace
check/tests, workspace-wide all-target/all-feature `-D warnings` Clippy, and the
release MCP binary build completed with exit code 0;
`target/release/atlas --version` reports 1.6.0. `atlas doctor` correctly rejects
the checkout's ignored pre-CFG-v3 database (found v2, expected v3) and prints
the documented rebuild guidance; the local index was deliberately not destroyed
or silently migrated. Local verification was macOS arm64; Linux and Windows
coverage remains the responsibility of the gated release matrix.

Path-level verification record for 1.6.0:

- Impacted paths: full/dataflow CFG extraction, CFG SQLite persistence,
  C# capability text, and the shared Focus/Index readers of persisted CFG facts.
  The real `Listener.ReceiveCallback` fixture starts from source, runs the full
  extraction pipeline, reads SQLite, and verifies exact deterministic `Goto`
  identity plus unreachable lexical fall-through.
- Public surfaces: all CLI/MCP/TUI/shared-pipeline tests passed; MCP V1 names,
  schemas, trace envelopes, capability level, and confidence remain unchanged.
- Intentionally unaffected: symbol/reference/dataflow IR and Schema V3 DDL did
  not change in the final C# increment. The version bump updates all 16
  workspace package lock entries and Atlas skill metadata.
- Adversarial boundaries: `goto case/default`, unresolved targets, and any C#
  function containing `finally`/`using` cleanup remain abrupt without a guessed
  label edge; positive forward/backward, comment-separated label, finally,
  using, and real-project cases are covered.
- Observed failures were the expected red tests before implementation and the
  stale ignored v2 development DB reported by doctor. No release-gate test
  remains failing. Cleanup-crossing C# goto, computed/PHP goto, and platform
  matrix execution outside macOS arm64 remain residual risks.

✅ Done for 1.6.1: verified on 2026-07-27. Formatting, workspace-wide
all-target/all-feature `-D warnings` Clippy, and the all-feature workspace test
suite completed with exit code 0; `target/release/atlas --version` reports 1.6.1
and `atlas doctor` passes for a freshly indexed checkout. Local verification was
macOS arm64; Linux and Windows coverage remains the responsibility of the gated
release matrix.

Path-level verification record for 1.6.1:

- Impacted paths: bulk-load index staging (`db::bulk_schema`), the SQLite read
  connection path (`db::store::{mod,lifecycle}`), function-pointer and callback
  edge construction (`graph::graph_builder`), summary dataflow loading
  (`analysis::summary`, `db::store::dataflow`), reference resolution strategies
  5/6 (`resolution::{lib,context}`), pipeline phase ordering
  (`filesync::index_{phases,pipeline_orchestrator}`), and progress rate
  reporting (`types::progress`).
- Result equivalence is the release gate, not a side note. Every optimization
  was A/B'd on a cold `.atlas` against the same checkout and accepted only when
  the resolution strategy distribution (`s1..s6`, `miss`, `total`), the strategy-6
  breakdown (`s6_exact`, `s6_fuzzy_prox`, `s6_fuzzy_global`), and `edges_built`
  were identical item by item. Final row counts were re-read directly with
  `sqlite3`: `symbol_edges`, `data_nodes`, and `dataflow_edges` match the 1.6.0
  baseline exactly.
- Public surfaces: unchanged. No MCP tool name, schema, trace envelope,
  capability level, confidence value, or SQLite schema version moved. The only
  new public database API is `Store::find_data_node_at_range` and
  `DataflowReader::find_dataflow_edges_by_function`, both additive.
- Adversarial boundaries: the summary path keeps the file-scoped
  `find_dataflow_edges_by_sources` fallback for units whose nodes carry no
  resolved `function_id`, so the new function-scoped join is used only where it
  is provably equivalent. Proximity fuzzy candidates are re-sorted so
  `sort_by_key` tie-breaking stays byte-identical to the old full linear scan.
  Read-pool slots fall back to the write connection for in-memory databases,
  preserving the single-connection reentrancy behaviour that unit tests rely on.
- Residual risks: the read pool multiplies open connections and page cache per
  `Store`; the per-connection cache budget is now divided by pool size with a
  16 MiB floor, but a container with a hard memory cap and many concurrently
  open projects should be measured. Edge building still has no cancellation
  checkpoint, so `Ctrl-C` during that phase waits for completion — far less
  visible now that the phase runs in seconds rather than minutes, but unfixed.

Pre-release TUI/MCP/Focus alignment review:

- Query cache authority is `PipelineGrade` + whole-repository finalized scope
  + fresh complete per-file coverage for the requested `QueryNeed`; display
  `catalog_tier` neither revokes manifest authority after partial Focus
  enrichment nor grants stronger authority. CallGraph/dataflow additionally
  require current canonical resolution fingerprints for reference-bearing
  files, so a source-changing Focus rebuild also revokes affected importers and
  cannot retain stale RepoCanonical provenance, while unrelated reference-free
  files do not force unnecessary Focus.
- `file_dependencies(manifest)` is a pure current-facts read;
  `analysis=structural` consistently requires CallGraph in contract and
  handler preparation. Failed Focus work remains failed in both the original
  query gate and `tasks`.
- TUI forms preserve handler-specific defaults and expose request-scoped
  `include_roots`; partial/scoped catalogs open symbol context through the
  shared focus-aware handler, while native graph views require finalized
  whole-project CallGraph coverage. Palette completion invalidates the native
  snapshot and refreshes shared Store status.
- MCP schemas and handlers share explicit hard bounds; oversized numeric
  arguments are clamped at the handler boundary, list/context responses expose
  returned/truncated metadata, and multi-hop calls bound the traversal frontier
  itself rather than only truncating the serialized vector. TUI list forms use
  the same optional limit arguments.
- Tier-0 inventory writes accept one typed `DiscoveredFile` record instead of
  seven positional discovery fields; the later content fingerprint phase
  remains a separate boundary.
- Focus-vs-full-Index dataflow/CFG parity now has a shared baseline matrix for
  all 14 languages. Go, C#, Rust, Ruby, Kotlin, Cangjie, PHP, and the
  TypeScript/JavaScript/ArkTS grammar family retain stronger feature-specific parity
  fixtures for their type-switch, switch-pattern, match, Ruby multiple
  assignment/modifier-loop, subject-binding, callable-variable-namespace, and
  direct-variable mutation boundaries.

### 1.6 Completed baseline release gates

The original baseline implementation blockers are closed and covered by the release test matrix:

- ✅ Workspace and MCP feature test suites compile and pass against the current schema and types.
- ✅ Shared `run_index_pipeline` owns deleted-file cleanup and persistent summary construction.
- ✅ Summary capability is persisted only after summary construction succeeds.
- ✅ Every language has an explicit top-level-only Manifest path.
- ✅ CLI rejects unknown `--analysis` values.
- ✅ Lazy-triggering MCP tools use the shared public analysis view, including no-result trace and CFG-consuming paths.
- ✅ Mixed Index + Focus catalogs preserve lower-layer authority per QueryNeed without promoting stronger queries.
- ✅ MCP responses remain valid single-document JSON; bounding occurs inside tool result structures.

## 2. Completed work

### 2.1 DataflowInterproc + persistent summary layer ✅

All 14 languages are now at `DataflowInterproc` level. The current schema added 4 persistent summary tables (`function_summaries`, `summary_param_reaches`, `summary_return_sources`, `summary_call_arg_sources`) with `CrossFunctionBridge` for ArgToParam/ReturnToCall interprocedural bridges.

> **Cross-language direct-variable mutation status (updated 2026-08)**:
> All 14 persisted language identities now preserve their supported
> direct-variable mutation forms without collapsing language identity. Cangjie
> additionally preserves direct simple reassignment and its direct-identifier
> non-conditional compound/postfix update forms; its pinned grammar/query shape is
> covered independently rather than inferred from another frontend.
> The whole expression is an aggregate read-modify-write value; previous target
> value and explicit RHS flow into it at 0.75, then it flows to the coalesced local
> at 0.90. Direct extraction、SQLite/Trace 与 cold Focus vs full Index cover every
> identity independently. Attribute/member/field/navigation、subscript/element/array/
> index、receiver 与 pointer/dereference targets、
> overloaded/dynamic operator semantics、
> numeric promotion/boxing、prefix/postfix result timing、`var` declaration/loop
> binding semantics、assignment destructuring、async scheduling and ArkUI callback/trailing-block
> internals remain conservative according to each language profile.
> TypeScript、JavaScript 与 ArkTS 的 direct-identifier
> `&&=`/`||=`/`??=` additionally preserve path-insensitive old-value/RHS
> may-provenance through Read 0.75 and Assign 0.90 without proving RHS execution or
> operator-specific truthiness/nullish control dependency. Ruby and Cangjie
> `||=`/`&&=` remain conditional-write boundaries.
> Unsupported compound targets do not degrade into an incorrect RHS-only write,
> synthetic local, or field store.

> **CFG status (updated 2026-08)**: CFG builder (`cfg_builder.rs`) exposes limited function/method CFG for all 14 languages. PHP branch/loop/switch/elseif, wrapped throw, return terminals, persisted E2E, golden, and the repository PHP syntax example are verified at WithLimitations(0.60). Switch sibling paths preserve C/C++/JS/TS/ArkTS/PHP and Java-colon implicit fall-through plus Go's explicit `fallthrough`; Go `select` additionally preserves communication/default siblings and blocking no-default semantics，真实 `gin.Context.Stream` 覆盖 SQLite persistence。`Break`/`Continue` edges resolve nested control paths, including PHP numeric nesting and Java、JS/TS/ArkTS、Go、Rust、Kotlin grammar-visible lexical labels；标签 target 可跨 finally/managed cleanup 后再由目标 loop/block 消费，真实 `ESLZ4Compressor.compress64k/compress` 覆盖 labeled break/continue 的 SQLite persistence。C/C++/Go/C#/PHP direct same-function goto/label 使用专用 `Goto` edge；C# 在 goto 退出时按内到外顺序经过 using `BlockExit` 与 path-isolated finally clone，禁止跳入更深的词法/cleanup region 或跳出 finally clause。PHP standalone label 复用 Join target，允许跳入普通 block 或跳出 loop/switch，禁止跳入 loop/switch 或跨 finally-clause 边界，退出 nested try 时按内到外经过 path-isolated finally clone。真实 Redis `hdr_percentiles_print` 与 Shadowsocks `Listener.ReceiveCallback` 覆盖 direct goto persistence，synthetic C#/PHP fixtures 覆盖 cleanup-crossing goto 的 extraction→SQLite persistence；未知/非直接目标终止本地 best-effort 路径。Go defer 在 64-clone 预算内按 CFG×runtime-stack 展开，条件注册不会串线，normal return 通过专用 `Defer` edge 和 owner-matched `BlockExit` 执行 LIFO cleanup；真实 Gin `Engine.RunUnix` 覆盖 early/final return 的不同持久化 defer stack。Rust `?` 保留 success continuation 与 residual return-to-Exit，并在 closure/async boundary 停止；Rust `let-else` 将 success 与显式 return/break/continue、unconditional-loop 或 unqualified builtin panic-like macro alternative 分离，真实 `Controller::print_file_ranges` 与 `EscapeSequenceOffsetsIterator::next_osc` 覆盖 SQLite persistence。Ruby modifier while/until 区分 plain pre-test 与 `begin...end` post-test，并复用既有 `next`/`redo`/`break` target 语义。C++ lowers try/catch and explicit throw through `Exception` edges；JavaScript/TypeScript/ArkTS、Java、C#、PHP、Python、Kotlin、Cangjie 和 Ruby 进一步以 path-isolated finally/ensure clones 表达 normal、return、throw、break 与 continue continuation。Java/C#/PHP direct object-created explicit throw 按源码顺序连接 handler，并在首个无 guard 的语法精确匹配处截断；真实 Elasticsearch `RestActions.getQueryContent` 覆盖 extraction→SQLite 边界。Java try-with-resources、C# using、Python with、Kotlin use 与 Ruby block resource 使用持久化 owner 匹配 normal/abrupt completion 的隔离 BlockExit，并确定性地按 LIFO 生成 cleanup；cleanup 自身抛出的异常会保留有序 `Throw` continuation，经过外层资源退出与 finally/ensure 后进入词法 handler。注释 AST extras 不生成可执行 Statement，包括 label 与正文之间的 comment extras。单个路径隔离区域超过 64 个 clone 时原子回退为 Statement。All return/throw terminals connect to the unique function Exit. Go cyclic/over-budget defer、panic/recover/Goexit 与复杂 anonymous deferred body、Rust macro shadowing/re-export、custom never-return macro 与 panic unwind/catch_unwind、ArkUI trailing blocks and nested arrow callbacks、Ruby ordinary iterator/callback blocks、cleanup exception suppression/replacement 与精确 identity、computed goto、C# `goto case/default`、C++ cross-scope destruction、grammar-hidden labels、继承/alias/变量/guard handler selection 和 implicit exceptions remain explicit boundaries.

> **ArkTS state-flow status (updated 2026-07)**: `AppStorage.set/setOrCreate` incoming flow is query-time `StateFlow`, with exact `this`-field and literal/expression key-category matching. Full-cache and cold Focus paths are both covered; cold Focus uses `StateChannel` closure discovery plus writer-function dataflow materialization on resume. Reverse `StorageLink`, constant evaluation, timing, and process boundaries remain explicit limitations.

> **ArkTS declaration status (updated 2026-07)**: TS-compatible abstract classes, interface properties/methods, enum members, async flags, and decorator references are extracted through existing IR. ArkUI false methods are rejected by ownership (`method_definition` must be a direct `class_body` member). A byte-stable declaration-only recovery tree rewrites complete fake component headers to valid `if(1)` headers and restores declarations swallowed after deep ArkUI blocks, including owning structs, post-build `@Styles`, following `@Builder`, and top-level `@Extend`; semantic facts remain on the primary tree. Field type/initializer have complete source spans but no dedicated structured IR.

> **ArkTS advancement triggers (updated 2026-07)**: Re-evaluate the ArkTS analysis boundary when any of the following is met: (1) real queries need to distinguish ArkUI conditional render paths; (2) a sink inside a callback has no explainable path via existing facts; (3) callback registration or callback lifecycle/branch analysis is needed; (4) a stable versioned grammar covering ArkUI declarative syntax exists. Before choosing between ArkTS/ArkUI tree-sitter grammar vs callback IR, build a baseline with real ArkTS corpus; capability must not be raised before implementation.

> **Cross-language advancement rule (updated 2026-08)**: ArkTS is not a special
> testing exception. For every language, promote a boundary only after a real
> query demonstrates that the current fact model cannot explain a required path,
> a stable grammar exposes the construct, and extraction→SQLite→consumer tests
> prove the new semantics. Before promotion, add negative/boundary fixtures when
> a limitation could otherwise look like a complete empty result. The next
> corpus-driven candidates are ArkUI callbacks, C++ cross-scope destruction,
> remaining language-specific guarded/pattern-binding dataflow, Rust runtime-length
> suffix projection/guard dependency, and Ruby structural pattern
> projection/post-match path-definedness; none is a pre-1.6.0 capability gate without such a fixture. Java
> `if`-condition/arrow-switch guarded patterns、Ruby retry/redo and Rust fixed
> tuple/tuple-struct/struct/slice-prefix projection graduated
> from this list after pinned grammar, adversarial extraction, SQLite consumer,
> and Focus-vs-Index evidence established their scoped semantics.

### 2.2 Index scope / manifest + Focus materialize

- **Scope Index** — `--include` / `--scope` / `--exclude`.
- **Manifest** — `ExtractionMode::Manifest` top-level symbols.
- **Focus materialize** — on-demand structural/dataflow under `FocusMaterialize`（机制类型可名 `Lazy*`；产品路径 Focus）。
- **Mixed catalogs** — a finalized whole-repo Index retains authority only for
  QueryNeeds covered by both its PipelineGrade and every file's fresh facts;
  partial Focus enrichment neither revokes lower facts nor promotes higher ones.

### 2.3 Workspace

`atlas-engine` facade + internal crates（含 `focus_materialize`）、`atlas-mcp`、`atlas-cli`。

### 2.4 Performance baseline features

PhaseTimings、hash dirty-set、thread-local parsers、batch DB writes、GlobalSymbolIndex、Rayon edges、on-demand dataflow/CFG、capability-driven skip。

### 2.5 Focus 可观测与恢复

- `FactCoverage` 在 `extraction_state`；MCP 公共面：`analysis` / `gaps` / `query_id`。
- `resume_query(query_id)`（session 内存 snapshot，短 TTL）。
- `Investigation`、`tasks`。
- 邻域 facts 对拍：`docs/testing.md` §2.6.2。

### 2.6 Field lifecycle, branch diff, and semantic impact ✅

- C/C++-oriented `FieldLifecycleEngine` analyzes field and local-resource transitions from
  CFG/dataflow facts; handlers compose semantic effects at query time rather than requiring
  pre-annotated persisted CFG nodes.
- Lifecycle transitions retain owner-bound true/false/case/exception branch contexts. An
  exception abandons frames owned by its try region, preserves enclosing conditions, and
  exposes the resulting context through MCP per transition.
- Built-in ownership classification includes common Linux kernel alloc/free APIs.
- `BranchDiffEngine` compares sibling branch side effects without introducing a separate Function IR.
- Lifecycle proof mode can use domain rules to raise evidence to rule-backed proof.
- `impact` can include semantic impact summaries based on lifecycle paths and domain rules.

### 2.7 Domain Rules generic layer ✅

- `domain_rules` crate provides language-agnostic rule storage, matching, registry validation, and learning candidate infrastructure.
- The `domain_rules` table includes `language`, `pattern_kind`, `meta`, `meta_version`, `status`, and timestamps.
- C/C++ ownership semantics live in `analysis::CppOwnershipRules`; the generic engine does not interpret ownership or lifecycle semantics.
- Language extension guidance is documented in `docs/domain-rules-language-guide.md`.

### 2.8 MCP tool consolidation (open-first focus surface) ✅

MCP 工具面已重构为 15 个 open-first 短名工具。`index`、`task_status`、`wait_for_task`、`resume_task` 和后台 open/search 参数不再属于 MCP；显式全项目索引只保留 CLI `atlas index`。

### 2.9 Large-repository focus correctness ✅

- Foreground graph preparation is seed-only; requested multi-hop expansion is a tracked,
  resumable background fixed point.
- Function-local semantic tools use a dedicated focus intent and do not enqueue unrelated
  call/type expansion.
- Type ranges across supported brace-based languages participate in the same stale-cache
  invariant and one-time self-healing path; persisted line intervals cannot be inverted.
- TUI native search is independent of graph snapshot readiness. Native detail/caller
  views require finalized whole-project CallGraph coverage; partial catalogs use the
  shared focus-aware MCP handler. Native snapshot loading and stale refresh run through
  the background job system.

## 3. Trace and language capability work

### 3.1 Capability alignment

- Keep `language_capabilities` and `atlas doctor` aligned with actual compiled features.
- For each language, maintain explicit limitations and confidence floors.
- Ensure unsupported or partial trace queries return diagnostics rather than silent empty results.
- Keep `FactCoverage` synchronized with persisted state: `cfg` requires actual CFG facts, `dataflow` requires dataflow facts, and `summaries` requires successfully built summary tables.
- `analysis.basis` may only advertise facts proven by DB state or verified during the current tool call.
- ✅ TypeScript、JavaScript、ArkTS、Java、C、C++、Go、Rust、Kotlin、Cangjie 的普通 block/sibling-block、PHP assignment/anonymous-function/`[]`/`list()` nested/keyed/by-reference destructuring/direct-variable mutation，以及 Ruby source-ordered block namespace
  scope-chain identity 已通过 extraction、SQLite Trace、Focus-vs-full-Index 三层
  回归，`scope_aware_binding` 与 adapter slot confidence 已对齐。Java 不伪造非法的
  overlapping local shadowing；PHP destructuring key expression 保持读取，assignment whole RHS 与 foreach collection 保守流向各 target，direct variable mutation 保留 aggregate read-modify-write provenance，exact key/index projection、missing-key/null、reference-alias semantics、dynamic/non-variable target、conditional write 与 prefix/postfix result timing 保持 limitation；未显式捕获的匿名函数外层 local 保持 unresolved，arrow-function ownership 暂不伪造；Ruby block write 仅复用源码更早的祖先 binding，flat/nested/rest multiple-assignment local target 复用同一 namespace，显式 RHS list 具备顶层位置流；Go same-block mixed `:=` 已覆盖 local/函数体参数复用、声明后激活与 clause 隔离；其余语言特有的 pattern、projection、mixed declaration、smart-cast 与
  definite-assignment 边界仍以 capability limitation 为准。
- ✅ C、C++、Java 与 C# direct-variable compound/update mutation 已与
  TypeScript-family 的 aggregate read-modify-write 契约对齐：direct extraction
  验证 binding coalescing、0.75 Read 与 0.90 Assign 及 unsupported target 负边界；
  SQLite/Trace 验证运算符位置选择 aggregate Expr 与输入来源；cold Focus 对 full
  Index 验证同 unit bindings/dataflow/CFG/confidence，并保持 peer unit 冷态。
- ✅ TypeScript、JavaScript 与 ArkTS direct-identifier `&&=`/`||=`/`??=`
  logical assignment 已在共享 pinned grammar 上显式建模为路径不敏感的
  old-value/RHS may-provenance：两种可能来源以 Read 0.75 进入 aggregate Expr，
  Expr 再以 Assign 0.90 进入 coalesced Local，但不证明 RHS 必然执行。
  Direct extraction、SQLite/Trace、cold Focus==full Index 与真实 OpenCode
  `defaultPreferred ??= select(process.env.SHELL)` 覆盖该边界；member/subscript
  target 与 operator-specific truthiness/nullish control dependency 保持保守。
- ✅ TypeScript、JavaScript 与 ArkTS 的 `let/const` declaration destructuring
  simple/renamed/nested/default/rest target 已使用 block-scoped binding；computed
  key 与 default RHS 保持读取。Whole initializer 以 Assign 0.85 向每个 target
  提供 aggregate provenance。Direct extraction、SQLite/Trace、cold Focus==full
  Index 与真实 OpenCode `toV1Message` 的 `{ id: _, sessionID: __, ...rest } = info`
  覆盖三种 identity。OpenCode 语料审计包含 2,201 行 direct `const` object/array
  declaration destructuring，分布于 771 个文件。Exact property/index projection、
  `var` declaration binding 与 assignment destructuring 保持保守。
- ✅ TypeScript、JavaScript 与 ArkTS 的 function/method/arrow parameter
  destructuring simple/renamed/nested/default/rest leaf 已使用 function-scoped
  Parameter binding；同一顶层 parameter 下的每个 leaf 共享同一调用位置，
  TypeScript 擦除的 `this` parameter 不消耗 runtime argument。Full Index summary
  与 cold Focus runtime 都将 whole call argument 以 aggregate `ArgToParam` 流向这些
  leaf；computed key 与 default RHS 保持读取。Direct extraction、SQLite/Trace、
  cold Focus==full Index 与真实 OpenCode `hasFunctionCall` 跨文件调用覆盖
  三种 identity。OpenCode 语料审计包含 978 个 destructured parameter，
  分布于 383 个文件；assignment destructuring 仅 1 处。Exact property/index
  projection 与 parameter default activation 保持保守。
- ✅ TypeScript、JavaScript 与 ArkTS 的 `let/const` `for-of`/`for-in`
  simple/nested pattern capture 已使用 loop-scoped binding；无 declaration 的
  direct existing-local assignment form 复用原 binding。Whole iterable/object
  以 Assign 0.65 向各 target 提供 aggregate provenance，`for-await` 只保留相同
  值来源契约，不声明异步调度。Direct extraction、SQLite/Trace、cold Focus==full
  Index 与真实 OpenCode `serializeSearchParams` 的 `[key, value] of entries`
  覆盖三种 identity。Exact element/key projection、`var` function-scoped binding
  与 member/subscript iteration target 保持保守。
- ✅ Cangjie simple/nested-tuple/enum-payload `for-in` capture 已建模为
  loop-scoped binding，enum constructor syntax 不建 binding，iterable 向每个 capture 以
  0.65 aggregate Assign 提供来源；guard/body 复用循环变量 identity，循环后同名 use
  恢复外层 binding。Direct extraction、SQLite Trace、Focus-vs-full-Index 已对拍；其他
  tuple/destructuring、resource binding、exact iterator element/结构投影与 pattern
  irrefutability 的编译器验证仍为显式边界。
- ✅ Go identifier-only select receive 已贯通既有 clause scope、BindingGraph 与
  DataFlow：`:=` target 在 communication clause 的 implicit block 中声明，`=` target
  复用 existing binding，whole receive operation 以 0.78 aggregate Assign 流向各
  supported target，blank identifier 不建事实。Direct extraction、SQLite Trace、
  Focus-vs-full-Index bindings/dataflow/CFG/confidence 已对拍；exact receive-result
  component、non-identifier receive target 与 parallel-assignment evaluation order 仍为边界。
- ✅ 删除 frontend slot 反向派生第二份 capability profile 的死路径；运行时能力门控只读
  `LanguageCapabilityProfile.features`，slot capability 仅作为实现契约与一致性守卫。

### 3.2 Path-level validation

Continue expanding end-to-end smoke tests for all languages.

- Add per-language Manifest validation fixtures that include both top-level and local declarations.
  ✅ Done: extraction tests now cover every `available_languages()` frontend
  with top-level symbols and nested/local rejects, enforce manifest-only output
  shape, and require `SymbolDef.layer` to match the manifest layer.
- Add shared-pipeline parity tests for Manifest, Structural, and Full against CLI index/sync behavior.
  ✅ Partial: `pipeline_equivalence` now covers shared `run_index_pipeline`
  versus structured `IndexPipeline::run` for Manifest, Structural, and Full
  DB state; CLI command and sync entry parity remain follow-up coverage.
- Add lazy dataflow tests for build, cache hit, full-index prebuilt cache, pending, partial, no-path trace, and CFG-consuming tool paths.

### 3.3 Public analysis view consistency

- Keep all lazy-triggering MCP tools aligned on `analysis`, structured `gaps`, and terminal retry semantics.
- Keep internal `FactCoverage` details behind the public `analysis.basis` and `gaps[].reason` boundary.
- Keep `query_id`, `resume_query`, and `tasks` behavior documented and covered by tests.
- No MCP response may return a semantic/CFG result while its contract says that same capability is unavailable.
- No recoverable lazy query may omit `query_id` or retry state solely because the current trace/search result is empty.

### 3.4 FP dispatches: struct function-pointer field indexing

`fp_dispatches` maps a struct function-pointer field (for example `rtnl_link_ops.changelink`) to a concrete target function via user annotation. C/C++ extraction now indexes parenthesized function-pointer fields such as `int (*do_it)(int)` as normal `Field` symbols, so this does not require a separate function-pointer-field entity or schema path.

The validated path is:

1. extraction emits `struct.field` / `Class::field` as `SymbolKind::Field`;
2. reference resolution can bind a field access such as `ops->do_it(...)` to that field symbol;
3. `fp_dispatches` stores the user annotation;
4. annotation materialization writes both the direct `field → target` edge and the caller bridge `caller → target` edge with `user_annotation` provenance.

**Remaining validation:** keep large-kernel smoke coverage for real tables such as `rtnl_link_ops`, `proto_ops`, and `file_operations`, especially initializer-heavy patterns and multi-file include/focus paths. Do not add new persistent entities unless a real fixture proves the existing field-symbol model cannot represent a needed dispatch.

## 4. Graph and performance evolution

### 4.1 Graph/dataflow/CFG loading

- Keep symbol graph snapshots as the main graph-query accelerator.
- Load dataflow and CFG facts by file/function/slice when trace queries need them.
- Avoid unbounded in-memory loading for fine-grained dataflow and CFG facts.

### 4.2 Performance targets

- Keep `docs/performance.md` updated with release baselines.
- Track index time, DB size, memory use, and MCP query latency.
- Prioritize resolution and DB write bottlenecks.

### 4.3 Large-file lazy extraction budget

Lazy structural extraction has a budget cap (~18s / 30 files for foreground, ~60s / 100 files for background). Very large source files (>2000-line functions like `copy_user_syms` in `kernel/trace/bpf_trace.c`) can exhaust this budget before completing structural extraction, causing tools (`calls`, `trace`, `explore`) to return bounded retryable responses until background refinement or a terminal gap resolves the query.

**Why**: Linux kernel has ~70 files with >10,000 lines and individual functions exceeding 2,000 lines. When an agent queries a symbol in one of these files, the lazy window processes the entire file (not just the target function). Tree-sitter parse + SCM query + dataflow/CFG build for a single huge file can independently exceed the per-window time budget, even when the file is the only unit in the window.

**Current mitigation**: Focus structural extraction now uses the enclosing `FocusWindow` wall-clock budget as one shared cancellation token instead of resetting a fresh 18s token per file. Foreground work remains bounded by the foreground window; background closures can use their wider window for a genuinely expensive file without inventing a new function-level structural store.

**Remaining validation**: Keep large-kernel smoke coverage for `calls`, `trace(forward)`, and `explore` on oversized files. If a real fixture still proves whole-file structural extraction cannot converge, prefer a measured extraction-slice design over adding another persistent indexing entity.

## 5. Public API stabilization

`atlas-engine` already exists as a facade crate. API stabilization proceeds from
the supported high-level entry points and their complete signature type closure:

- ✅ Top-level facade no longer re-exports zero-call `phase_*`,
  `run_index_pipeline`, dirty/cleanup helpers, `ClosurePlanner` worksets,
  parser-pool internals, summary persistence internals, or resolution-session
  helpers. Stable Engine/Index/Graph/Search/Workspace entries re-export the
  argument and return types needed to name their public signatures.
- ✅ Unused `JobContext` and dead ClosurePlanner workset/sibling/regex-bootstrap
  paths were deleted instead of hidden behind compatibility aliases or
  `allow(dead_code)`.
- Remaining: `analysis`, `trace`, `dossier`, `rule_engine`, and Focus control
  modules are still ordinary `pub` because MCP/CLI sibling crates consume them.
  Narrowing these requires moving complete use cases to an owning engine/leaf
  boundary; `pub(crate)` on the facade is not a valid cross-crate solution.
- Avoid leaking internal schema details unnecessarily.
- Document feature flags and language availability.
- Keep CLI/MCP as consumers of the same engine behavior.
- Lock trace response contracts before promising downstream compatibility.
- Keep the current 15 short-name MCP tools stable; new tools require a distinct user need and contract tests rather than aliases or prefixed duplicates.

## 6. Future product lines

### 6.1 Atlas mainline

- Indexing and incremental sync.
- Symbol graph and dependency graph.
- Search/context/impact analysis.
- Variable provenance and caller-path tracing.
- MCP-driven agent context.

### 6.2 Corpus (separate product line)

A multi-version source corpus system for Linux/U-Boot/BusyBox-style repositories remains a separate future product line:

```text
Atlas:  project-relative path + local workspace DB
Corpus: git blob + version/tag/path mappings
```

## 7. Not planned for the current Atlas mainline

- Full compiler-grade type checking.
- Full C/C++ preprocessing, template instantiation, overload resolution, or alias analysis.
- Full Python dynamic/runtime symbol resolution.
- Java Maven/Gradle/classpath completeness.
- Automatic vulnerability scanning, taint rules, finding generation, or SAST product features.
- Multi-version source corpus indexing.
- Full compiler-grade C/C++ ownership proof, pointer arithmetic, union aliasing, or complete cross-function dataflow.

## 8. Semantic analysis status and remaining work

### 8.1 Current architecture ✅

The multi-effect semantic pipeline is implemented:

```text
CFG + DataFlow
  → EffectComposer (multiple SemanticEffect values per CFG node)
  → language ResourceOpConfig / domain-rule consumer
  → lifecycle state transfer, branch_diff, lifecycle proof, semantic impact
```

- `EffectComposer` traces value flow such as alloc → local → field and emits multiple effects with provenance.
- Lifecycle uses a path-sensitive field-state lattice and consumes composed effects.
- Branch diff compares semantic effects across sibling branches rather than raw statement counts.
- Resource-operation registries cover C/C++, Rust, Go, Python, TypeScript, Java, C#, Kotlin, PHP, and Ruby patterns; language-specific meaning stays outside the generic domain-rules core.
- Capability profiles expose limited CFG for all 14 languages; precision differs by language and construct.

### 8.2 Remaining precision work

- Expand golden/end-to-end fixtures for nested branches, switch/match, exceptional control flow, async boundaries, and language-specific resource constructs.
- **PHP array destructuring — scoped phase implemented:** `[]` and `list()`
  assignment/`foreach` nested、keyed 与 by-reference variable targets join the
  existing callable namespace；key expressions remain reads. Assignment whole
  RHS and foreach collection conservatively flow to every supported target
  through existing `Assign` edges. Direct extraction、SQLite Trace and
  Focus-vs-full-Index fixtures cover identity and product-path parity. Exact
  key/index projection、missing-key/null behavior、reference-alias semantics
  and dynamic/non-variable targets remain explicit precision boundaries.
- **PHP direct-variable mutation — scoped phase implemented:** file/function/
  method namespace variables used by `op=` and prefix/postfix `++`/`--`
  produce an aggregate read-modify-write Expr. The previous value and explicit
  RHS enter that Expr through `Read`; the Expr reaches the coalesced Local write
  through `Assign` at 0.90. Direct extraction、SQLite Trace and
  Focus-vs-full-Index fixtures cover persisted binding identity、edge confidence、
  CFG parity and cold peer isolation. Dynamic/non-variable mutation targets、
  `??=` conditional execution and prefix/postfix result timing remain explicit
  precision boundaries.
- **TypeScript-family logical assignment — scoped phase implemented:** direct
  identifier `&&=`/`||=`/`??=` preserves both the previous local value and the
  conditional RHS as path-insensitive possible origins of one aggregate Expr
  through `Read` at 0.75；the Expr reaches the coalesced Local through `Assign` at
  0.90. This is a may-provenance contract, not proof that the RHS executes.
  Direct extraction、SQLite/Trace、cold Focus-vs-full-Index and the real OpenCode
  `defaultPreferred ??= select(process.env.SHELL)` fixture cover TypeScript、
  JavaScript and ArkTS identities independently. Member/subscript targets and
  exact truthiness/nullish control dependency remain explicit boundaries.
- **TypeScript-family declaration destructuring — scoped phase implemented:**
  broad pinned-grammar leaf captures are accepted only when recursive pattern
  classification proves they belong to the `name` field of a `let/const`
  `variable_declarator`. Simple、renamed、nested、default-left and rest targets
  join the enclosing block scope；computed keys and default RHS expressions stay
  reads. The whole initializer reaches every supported target through aggregate
  `Assign` at 0.85. Direct extraction、SQLite/Trace、cold Focus-vs-full-Index and
  the real OpenCode `{ id: _, sessionID: __, ...rest } = info` fixture cover
  TypeScript、JavaScript and ArkTS identities independently. Exact property/index
  projection、`var` declaration binding and assignment destructuring
  remain explicit precision boundaries.
- **TypeScript-family parameter destructuring — scoped phase implemented:**
  recursive pattern classification accepts simple、renamed、nested、default-left
  and rest leaves only inside function、method or arrow parameters. Every leaf is
  a function-scoped Parameter and persists the shared top-level runtime argument
  position; the erased TypeScript `this` parameter does not consume a position.
  Full Index summaries and cold Focus runtime edges both map the whole call
  argument to every leaf through aggregate `ArgToParam`. Computed keys and default
  RHS expressions remain reads. Direct extraction、SQLite/Trace、cold
  Focus-vs-full-Index and the real cross-file OpenCode `hasFunctionCall` fixture
  cover TypeScript、JavaScript and ArkTS identities independently. The audited
  corpus contains 978 destructured parameters across 383 files, while assignment
  destructuring appears once. Exact property/index projection and parameter
  default activation remain explicit precision boundaries.
- **TypeScript-family iteration binding — scoped phase implemented:** pinned
  `for_in_statement` fields drive one shared TS-family path. `let/const`
  simple/nested pattern leaves join the loop scope；direct existing-local
  assignment forms reuse their prior binding. The whole iterable/object reaches
  each supported target through aggregate `Assign` at 0.65, including the value
  provenance portion of `for-await`. Direct extraction、SQLite/Trace、cold
  Focus-vs-full-Index and the real OpenCode `[key, value] of entries` fixture cover
  TypeScript、JavaScript and ArkTS identities independently. Exact element/key
  projection、`var` function-scoped binding、member/subscript iteration target and
  async scheduling remain explicit precision boundaries.
- **Python/Go/Rust/Kotlin/Ruby direct-variable mutation — scoped phase implemented:**
  each pinned grammar now emits one direct-identifier aggregate read-modify-write
  shape for its supported augmented/compound/operator/update syntax. Previous value
  and explicit RHS enter the Expr through `Read` at 0.75；the Expr reaches the
  coalesced Local through `Assign` at 0.90. Direct extraction、SQLite/Trace operator
  selection and cold Focus-vs-full-Index fixtures cover all five persisted language
  identities, including RHS-only-write suppression and unsupported-target negative
  boundaries.
- **Cangjie assignment mutation — scoped phase implemented:** direct simple
  reassignment preserves RHS→Local provenance at 0.90 without treating its LHS as a
  read；direct-identifier non-conditional compound/postfix update preserves previous
  value/explicit RHS→aggregate Expr at 0.75 and Expr→Local at 0.90. Direct extraction、
  SQLite/Trace operator selection and cold Focus-vs-full-Index fixtures cover the
  persisted Cangjie identity. Field/index targets、`&&=`/`||=` conditional execution、
  operator dispatch/coercion and prefix/update-result timing remain explicit
  precision boundaries.
- **Ruby multiple assignment — scoped phase implemented:** flat/nested/rest local
  targets join the existing source-ordered method/module/class/block binding
  namespace. Explicit RHS lists map to top-level target groups by position；a
  single aggregate RHS, nested destructuring, and rest targets use conservative
  group/slice flow through existing `Assign`/`FieldStore` edges. Direct
  extraction、SQLite Trace and Focus-vs-full-Index fixtures cover binding
  identity and product-path parity. Structural element projection, `to_ary`
  coercion, implicit `nil` fill, parallel evaluation order, and numbered
  parameters remain explicit precision boundaries.
- **C# parenthesized nested designation — scoped phase implemented:** switch
  arms recursively bind `var (first, (second, third))` designations with one
  arm-local identity per name. Direct captures retain 0.80 whole-subject flow;
  nested designation/property/list captures use 0.72 aggregate subject flow
  through existing `Assign` edges. Direct extraction、SQLite Trace and
  Focus-vs-full-Index fixtures cover persisted identity、confidence and cold
  unit isolation. Exact property/index/positional projection、compiler
  definite-assignment and guard control dependency remain explicit boundaries.
- **Kotlin late-assignment provenance — scoped phase implemented:** a typed
  local declared without an initializer remains a lexical binding only；later
  simple `=` writes use that identity and retain every concrete RHS across an
  exhaustive branch join. Direct extraction、SQLite Trace and
  Focus-vs-full-Index fixtures verify real-source tracing and cold-unit parity.
  Kotlin compiler variable-initialization analysis still requires an
  assigned/unassigned lattice over every CFG path；Atlas does not claim that
  proof, smart-cast type refinement、contracts、type/range projection or guard
  control dependency.
- **Java guarded pattern provenance — scoped phase implemented:** direct and
  record captures in supported `if`-condition `instanceof` expressions and
  Java 21 arrow switch rules use existing scoped bindings. Each arrow rule
  retains an independent identity, while the tested value or switch selector
  flows conservatively to every supported capture through existing `Assign`
  edges at confidence 0.75. Direct extraction、SQLite Trace and cold
  Focus-vs-full-Index fixtures cover declaration/use identity、nested record
  captures、constructor/underscore rejection、edge confidence and peer-unit
  isolation. Colon-style switch groups、standalone or other flow-sensitive
  boolean contexts、exact record-component projection、compiler
  definite-assignment and guard control dependency remain explicit boundaries.
- **Switch/match/select sibling detection** — *Phase 1 implemented*: `CfgBuilder::walk_switch` emits a `Branch` dispatch node with one `CfgEdgeKind::CaseBranch` edge per executable sibling into a shared `Join`. Languages wired up: C/C++、Java、JS/TS/ArkTS、Go switch、C#、PHP、Python、Rust、Kotlin、Cangjie，以及 Ruby classic `case` 和 `case ... in`。Go `select_statement` maps communication/default siblings while preserving blocking no-default semantics。Python unguarded syntax-irrefutable wildcard/capture/`as`/group/OR arms, Ruby unguarded capture/wildcard, plus Rust/Cangjie direct unguarded wildcards suppress impossible synthetic no-match paths；Ruby refutable case/in without `else` instead emits the required implicit Throw；Python match subject conservatively flows to capture/`as`/star bindings，Ruby case/in subject conservatively flows to bare/key-only/`=>`/rest bindings；两者的 guard/body uses 均复用 enclosing namespace identity，Ruby pin 保持读取。Kotlin nested control scope 的同名 local 经 scope chain 保留独立 identity，`when (val V = E)` 将 initializer 流向 subject binding，condition/guard/body 复用同一 scoped identity；typed late-declared local 的 simple `=` 写保留跨 branch join 的全部具体 RHS，但不证明所有路径均已赋值；Rust scrutinee 与 source-ordered guard-let RHS 对 fixed tuple/tuple-struct/struct/slice-prefix capture 建立 FieldLoad 0.80→Assign 0.90 projection chain，guard/body 复用 arm-scoped identity；bare/`@` whole capture 与 `..` 后 target 保留 aggregate Assign 0.75；Cangjie selector 保守流向 arm-scoped simple/tuple/enum-payload/type binding，guard/body 复用该 identity；Go type-switch guard value 保守流向每个 case/default implicit block 的 clause-local alias，guard alias 本身不作为读取；same-block mixed `:=` 复用 source-earlier local/函数体参数，新名字在声明后激活，switch/select clause 保持 sibling identity；identifier-only select receive 的 `:=` target 为 clause-local declaration、`=` target 复用 existing binding，whole receive operation 以 0.78 aggregate flow 流向各 target；Java `if` condition 的 direct/record `instanceof` 与 arrow switch type/record capture 使用 scoped identity，tested value/selector 以 0.75 aggregate flow 流向 capture；C# `is`/switch direct declaration/recursive/var pattern capture 采用 scope-chain identity，switch sibling arm 的同名 capture 不串线，subject 保守流向 capture。真实 `gin.Context.Stream`、Go `context.stringify`、Rust `parse_less_version_busybox`、`ClosurePlanner::plan_dependencies` 与 Cangjie `handleCommand`，以及 synthetic Go/Python/Ruby/Kotlin/Rust/Cangjie/Java/C# fixtures 覆盖 extraction→SQLite persistence/Trace。Remaining structural/type-driven pattern exhaustiveness、Python/Ruby structural projection/post-match path-definedness、remaining other-language guard/binding dataflow、Java colon-group/other boolean-context flow scope、exact record projection/definite-assignment/guard dependency、Go case-type projection/function-literal ownership/exact receive-result component/non-identifier receive target/parallel-assignment evaluation order、C# nested designation projection/definite-assignment/guard control dependency、Kotlin smart-cast/compiler-grade variable-initialization proof/type/range condition projection/guard control dependency、Rust runtime-length suffix projection/borrow/move/guard control dependency、Cangjie structural projection/guard control dependency 与 communication readiness probabilities remain outside CFG.
  - **Fall-through and control transfers — Phase 2 implemented:** C/C++/JS/TS/ArkTS/PHP and Java colon groups connect a reachable case tail to the next executable case; Java arrow rules and non-C sibling constructs terminate their arms; Go connects only an explicit `fallthrough`. Empty arms preserve the same per-language routing without synthetic executable nodes: implicit-fall-through labels target the next executable body, while Go switch/select、Java arrow、Rust/Kotlin/Cangjie/Ruby-style arms target the Join. Identical direct paths collapse to one `(source, target, kind)` edge. Exact `default`/`else` clauses suppress impossible synthetic no-match edges. Only C/C++/JS/TS/ArkTS/Java/Go/C#/PHP switch-like constructs consume unlabeled `break`；Python/Rust/Kotlin/Cangjie/Ruby sibling constructs propagate it to an enclosing loop or labeled block. `break`/`continue` use persisted `Break`/`Continue` edge kinds；PHP `break N`/`continue N` decrements through nested switch/loop levels. The real curl `convert_char` example and persisted JS/PHP/Ruby fixtures cover fall-through、break ownership and SQLite edge-ID integrity.
    - Both branch-diff engines now walk downstream case effects, so `case 1: log(); case 2: free(x); break;` attributes the free to both runtime entry paths. The conservative all-but-one rule remains: effect-less paths are ignored, a finding requires at least three effectful paths, and a resource unique to one case is treated as intentional.
    - Lifecycle branch frames bind to their matching Join ID. A nested `break` can unwind inner conditional and case frames while preserving an outer branch frame; fall-through keeps the active case context until the switch Join.
    - Adversarial grammar tests also closed wrapper/classification gaps outside switch: Ruby `do`/`then` bodies and Kotlin `control_structure_body` are traversed, Ruby `next` resolves as `Continue`, and Kotlin/Cangjie `break` is no longer misclassified as `Return`.
    - Exact lexical labels now resolve `break`/`continue` for Java、JS/TS/ArkTS、Go、Rust 与 Kotlin, including nested target selection and continuation through finally/managed cleanup. Java labeled blocks and Rust labeled blocks resolve break without emitting a fake statement. C/C++/Go direct same-function goto/label resolves forward and backward jumps through dedicated `Goto` edges；C# 在 goto 退出时按内到外顺序经过 using `BlockExit` 和 path-isolated finally clone，同一 cleanup region 内不提前执行清理，跳入更深的词法/cleanup region 或跳出 finally clause 保持非法且不生成猜测边。PHP standalone label 复用 Join target；goto 可跳入普通 block 或跳出 loop/switch，禁止跳入 loop/switch 或跨 finally-clause 边界，退出 nested try 时按内到外经过 path-isolated finally clone。Lifecycle clears active branch frames at the target instead of retaining conditions the jump may have invalidated，branch-diff 只枚举 Entry 可达的 Branch，避免为查找后置 label 而保留的 disconnected syntax 产生假告警。Real Elasticsearch `ESLZ4Compressor.compress64k/compress` covers labeled break/continue, Redis `hdr_percentiles_print` covers cleanup goto, Shadowsocks `Listener.ReceiveCallback` covers a comment-separated C# label, and synthetic C#/PHP fixtures cover nested cleanup-crossing goto through SQLite persistence. Computed goto、C# `goto case/default`、C++ cross-scope destruction, unknown labels, and labels hidden by the selected grammar remain deferred; unresolved transfers terminate the local best-effort path rather than being connected to a guessed target.
    - Rust `?` now adds a residual return-to-Exit continuation without removing the containing statement/control header's success path. Nested closure、async block 与 nested function stop recursive detection, and explicit `return foo()?` still emits one Exit edge. Real `examples/rust_example/src/line_range.rs::parse_range` covers extraction→SQLite persistence.
    - Rust `let-else` now evaluates its value on a `Branch`, routes the successful pattern path to a Join, and walks the alternative so explicit return/break/continue, an unconditional `loop`, or unqualified builtin `panic!`/`unreachable!`/`todo!`/`unimplemented!` remains abrupt. A `?` in the value adds an independent residual return path. Real `examples/rust_example/src/controller.rs::Controller::print_file_ranges` verifies that the alternative `break` cannot reach the following declaration, and `EscapeSequenceOffsetsIterator::next_osc` verifies a persisted panic match arm. Macro shadowing/re-exports, custom never-return macros, panic unwinding, and `catch_unwind` remain conservative.
    - Rust `match` gives each arm an isolated lexical scope and canonicalizes valid or-pattern bindings to the first alternative. Scrutinee and source-ordered guard-let RHS values now reach fixed tuple/tuple-struct/struct/slice-prefix captures through anonymous Expr access-path projections: value→projection is `FieldLoad` 0.80 and projection→capture is `Assign` 0.90. Guard/body uses reuse that arm identity; constructor/type syntax and non-canonical alternatives remain declarations-free. Bare/`@` whole captures and targets after `..` retain aggregate `Assign` 0.75 because their runtime component cannot be proven syntactically. Runtime-length suffix projection、borrow/move mode、guard control dependency 与单段常量的语法歧义保持保守。Synthetic fixtures cover nested ownership、adversarial pattern classification、SQLite Trace and Focus-vs-full-Index parity；`ClosurePlanner::plan_dependencies` provides a real nested `Result<Option<_>>` `[0][0]` persistence/Trace fixture.
    - Ruby 2.7+ `case ... in` (`case_match`/`in_clause`) now has non-fall-through sibling CFG. A refutable expression without `else` emits an implicit Throw for `NoMatchingPatternError`; direct unguarded capture/wildcard patterns suppress that impossible path. Bare/key-only/`=>`/array-rest/hash-rest captures receive conservative subject Assign edges in the enclosing local namespace；guards/bodies reuse that identity and pins remain reads。Classic Ruby `case`/`when`, Python, Rust, Kotlin, and Cangjie sibling paths remain wired. Python recognizes syntax-irrefutable unguarded wildcard/capture/`as`/group/OR patterns and models capture/`as`/star binding dataflow from the subject；Kotlin `when (val V = E)` models initializer→subject binding dataflow and shared condition/guard/body identity，typed late-declared local 的 concrete simple-assignment origins 可跨 branch join 回溯；Rust models fixed syntactic tuple/tuple-struct/struct/slice-prefix projection from scrutinee/guard-let values to arm-scoped captures plus shared later-guard/body identity；Cangjie models selector→arm-scoped simple/tuple/enum-payload/type binding flow plus shared guard/body identity；Java supported `if`-condition `instanceof` 与 arrow switch type/record capture 接收 tested value/selector 的 0.75 aggregate flow，并保留 switch-rule identity；Rust/Cangjie recognize direct unguarded wildcards for exhaustiveness. Remaining range/structural/type-driven exhaustiveness、Python/Ruby structural projection/post-match path-definedness、Kotlin smart-cast/compiler-grade variable-initialization proof/type/range condition projection/guard control dependency、Rust runtime-length suffix projection/borrow/move/guard control dependency、Cangjie structural projection/guard control dependency、Java colon-group/other boolean-context flow scope/exact record projection/definite-assignment/guard dependency and remaining language-specific guard/binding dataflow stay outside CFG/dataflow.
- **Exceptional control flow — Phase 1 implemented:** configured try grammars and Ruby method-body/nested `rescue` build a try dispatch `Branch`, Normal path, one `Exception` path per catch/except/rescue handler, and a shared `Join`. Explicit Throw/Raise nodes retain an uncaught continuation. Java/C#/PHP 的 direct object-created explicit throw 按源码顺序连接 handler，并在首个无 guard 的语法精确类型匹配处截断；更早的不同类型 handler 因继承关系未知而保留。变量、alias、filter/guard、constructor-like call，以及 Python/Kotlin/C++/Ruby 的 thrown identity 保持全 handler 保守连接；try dispatch 也继续覆盖所有 handler，以表示 implicit/unknown exception。真实 Elasticsearch `RestActions.getQueryContent` 已覆盖 extraction→SQLite persistence。Python `try/except/else` and Ruby `rescue/else` keep else on the Normal path. Exception edges are traversable CFG facts. C/C++ lifecycle transitions attach each handler to its try-dispatch owner with an `ExceptionPath` frame, discard conditions abandoned inside that try, preserve enclosing branch frames, and remove the exception frame at the matching Join；真实 Redis/jemalloc `handleOOM` 覆盖 extraction→SQLite persistence→analysis 边界。MCP 在每个 transition 的 `branch_context` 中暴露这些条件。Branch diff deliberately does not compare handlers as ordinary true/false siblings.
  - **Finally continuations — Phase 2 implemented:** JavaScript/TypeScript/ArkTS, Java, C#, PHP, Python, Kotlin, Cangjie, and Ruby clone the finally/ensure AST once per incoming normal, caught-exception, return, throw, break, or continue continuation. Ruby additionally preserves `redo` and unowned `retry` through nested ensure clones: abrupt ensure completion overrides them, while a rescue-owned `retry` bypasses that same rescued begin's ensure before restarting. Ruby handles both method-body implicit begin and nested begin. Clones retain exact source ranges and receive deterministic lowering-instance node IDs, so persistence keeps every path while consumers can still join facts by source range. Abrupt completion inside finally/ensure overrides the incoming continuation; nested throws resume an enclosing catch only after the inner finally/ensure completes. Lifecycle matches Branch/Join pairs through graph-local edges rather than globally ambiguous source ranges.
  - **Managed-resource continuations — Phase 2 implemented:** Java try-with-resources、C# using、Python with、Kotlin use 与 Ruby block resources 将 normal、return、throw、break、continue 分别路由到独立 BlockExit，C# 另将跳出 using 的 goto continuation 路由到独立 BlockExit，Ruby `retry` 也先经过资源 BlockExit。资源获取节点与所有出口共享持久化 lexical owner，consumer 只在 owner 相同的出口生成 `ContextManaged` Free，因此 nested/sibling scope 不串线；多个资源按 CFG source range 与 effect order 的逆序生成确定性 LIFO cleanup。每个非 Throw completion 同时保留 cleanup 自身抛错的有序 `Throw` continuation，nested resources 先经过外层 BlockExit，再进入词法 handler/finally/ensure；catch body 内的 cleanup 异常不会回入同一个 handler。Ruby block-level `break/next` 的成功出口在 cleanup 后汇合为 yielding call 的普通后继，不误交给外层 loop；`redo` 保持在 resource lifetime 内并直接回到当前 block body entry。Java try-with-resources 继续复用 try continuation lowering，显式 throw 与 return-path cleanup throw 均在 managed exit 后进入 catch，normal/abrupt managed exit 再进入各自的 finally clone；真实 Java `Jar.extractToTmp`、C# nested using 与持久化 Python/Ruby fixture 已验证。
  - **Bounded degradation:** one try/finally or managed-resource region may create at most 64 path-isolated clones. Over-budget input rolls back all nodes, edges, terminal queues, control-transfer queues, call context, and managed ownership emitted for that region, then persists one opaque Statement. This prevents adversarial CFG multiplication without publishing a partial graph.
  - **Ruby retry/redo and modifier loops — scoped phase implemented:** dedicated persisted `Redo` edges restart lexical while/until/for、modifier while/until or modeled resource-block body entry without reevaluating the condition/iterator call. Plain modifier loops test before body entry；`begin...end while/until` enters the body before the first test；`next`/`redo`/`break` resolve to condition/body/join. Dedicated `Retry` edges restart the begin dispatch owned by the nearest rescue；nested ensure/resource cleanup runs first, but the ensure belonging to the retried begin does not run before restart. Abrupt cleanup overrides the incoming continuation. Ruby `if_modifier`/`unless_modifier` bodies are traversed, with `unless` using the false condition edge. Adversarial extraction、SQLite persistence and Focus-vs-Index CFG-edge parity fixtures cover deterministic behavior. Ordinary iterator/callback block bodies remain outside CFG.
  - Remaining boundaries: resolved inheritance/alias catch selection and ambiguous thrown values, implicit exceptions from ordinary statements, Ruby ordinary iterator/callback block bodies, cleanup exception suppression/replacement and exact exception identity, computed goto, C# `goto case/default`, C++ cross-scope destruction and grammar-hidden labels, exception-handler sibling effect comparison, and full multi-path lifecycle proof conditions.

- **Go exit semantics — bounded normal-exit phase implemented:** `go` and `defer` call contexts are persisted. The CFG forms a bounded product with the runtime defer stack: the same lexical continuation reached with different registrations receives distinct deterministic node identities, and every normal function exit executes owner-matched `BlockExit` nodes in LIFO order through persisted `Defer` edges. Deferred resource consumption moves from the registration node to the matching execution node; resource-consuming nested call arguments remain immediate because Go evaluates function values and arguments at registration. Real Gin `Engine.RunUnix` verifies that its `net.Listen` error return executes only the leading defer while the final return executes `os.Remove → listener.Close → debug` through SQLite persistence. A stack that can grow through a loop, or expansion beyond 64 clones, falls back atomically to the annotated base CFG. Panic/recover/Goexit unwinding, deferred-call panic replacement, and complete effect ordering inside complex anonymous deferred bodies remain explicit boundaries.

- **Cross-function lifecycle tracking**: `lifecycle` currently tracks field transitions only within the queried function (intra-procedural). A common C vulnerability pattern is `alloc() in function_A` → `free() in function_B` — the lifecycle tool cannot detect mismatches across this boundary because CFG + dataflow facts are file-scoped. A bounded cross-function extension would compose call path edges with intra-procedural summaries to answer "is this pointer freed along all call paths?" at 1-2 call depths.
- Improve alias/value provenance where tree-sitter facts cannot distinguish same-name or dynamic targets.
- Keep semantic conclusions explicitly bounded by CFG/dataflow coverage, confidence, domain-rule provenance, and terminal gaps.
- Do not introduce a second Function IR unless CFG/dataflow facts demonstrably cannot express a required invariant.

Not in scope: SAST-style taint scanning, complete pointer provenance, compiler-grade lifetime verification, or automatic vulnerability findings.

## 9. Focus Runtime — 查询时控制平面演进

### 9.1 目标

Focus 是 Lazy Index 的下一个控制平面。Lazy 负责按需构建 facts；Focus 负责围绕用户意图
决定构建哪些 facts、按什么顺序、在哪个 closure scope 中可见。

核心原则：
- Focus 是内部基础设施，零用户可见表面。无 CLI 命令、无手动预热、无可视化面板。
- 项目无 full index 时静默自动激活。
- MCP 查询经 `QueryIntent → FocusRuntime::prepare` 统一入口，不再直接组合 lazy
  structural/dataflow、resolver 或 graph builder。

### 9.2 已完成阶段

| 阶段 | 内容 | 实现 |
|------|------|------|
| Phase 0 | 内部 precision 收敛 | extraction/focus 内部使用结构化 precision；MCP 公共边界不暴露内部 precision |
| Phase 1 | Bootstrap 冷启动 | `BootstrapManager`（Tier0 文件清单/Tier0.5 指纹/Tier1 SymbolHints/Tier2 机会性 manifest） |
| Phase 2 | FocusRuntime + QueryIntent | `QueryIntent → FocusRuntime::prepare` 统一入口；`QueryRuntime` 封装 MCP 集成 |
| Phase 3 | ClosureEngine | 策略驱动的有限不动点闭包扩展（ImportNeighborhood/CallGraph/TypeGraph/StateChannel），含预算控制 |
| Phase 4 | ScopedResolver + FocusGraphBuilder | 闭包作用域引用解析和 scoped graph overlay |
| Phase 5 | MCP Response Envelope 统一 | `analysis`/`coverage_counts`/`gaps`/`query_id` 统一 public view，删除 `precision`/`work` 等伪信号 |
| Phase 6 | 控制平面 | MCP 经 `FocusRuntime` + `FocusMaterialize`；无独立 lazy 控制面 |
| Phase 7 | 冷启动闭包正确性 | 精确 symbol frontier、dependency resolution-only、深度驱动 fixed point、后台 materialization refresh、成功/失败终态、完整 C/C++ type ranges 和旧 type-range cache 自愈 |

### 9.3 剩余工作

- 长期：继续收敛 extraction/focus 内部 precision 类型，保持 MCP 公共边界稳定且最小。
- 长期：以真实大型仓库 smoke 和受控 fixtures 持续测量 cold incoming candidate discovery；
  只有测量证明现有 bounded provider 不足时才引入新的索引实体。
- **Include-header structs in focus closure** — *foreground/background path implemented*: request-scoped `include_roots` thread from the MCP tool boundary through `prepare_focus_query_with_roots` → `QueryRuntime::prepare` → `FocusRuntime::prepare`, then are copied onto each `FocusWindow`. `ClosureEngine` no longer stores mutable per-query roots; `materialize_import_dependencies` reads roots from the window, so foreground closures, scheduled background closures, and hot-region extension windows all use the roots of the query that created them. Non-request prewarming still carries an empty roots vector by design.
  - **Remaining validation:** keep C/C++ angle-include fixtures and large-repo smoke coverage for `search`, `symbol(detail/usages)`, `context`, `calls`, `explore`, `trace`, `path`, `lifecycle`, and `branch_diff`; avoid reintroducing mutable include roots on cached engines.

### 9.4 不变边界

机制层：`LazyStructuralService`、`LazyDataflowService`、`ExtractionMode`、`extraction_state`、`extraction_jobs`。  
产品层：Index / Focus。构造：`FocusMaterialize::open`。  
详见 [`architecture.md`](./architecture.md) §2.1.1 / §7.1 / §10.1.11。

### 9.5 MCP DEBT-8 analysis dispatcher ✅（analysis 路径实质达成）

**已完成（当前事实）：**
- `AnalysisRuntime` 为 `lifecycle` / `branch_diff` / semantic impact 真 dispatcher（能力门控、dataflow I/O、compose、rules、engine）；`graph.rs` 只提供 impact 子图目标。
- `handler_purity` 双层守卫：engine 名 + orchestration 模式；allowlist 残量 1（`active_project.rs` project-open 工厂，合法例外）且必须有真实命中；残量上限 `assert!(allowlist.len() <= 1)`。
- god-router（`tools/mod.rs`）已迁出 allowlist：`focus_runtime` 字段私有，`focus_runtime.lock()` 直连消除，统一走 `QueryRuntime` 委托（`enqueue_file_focus_warm` / `focus_materialize_*`）。
- annotation 测试 seed 已改走 `overlay_runtime`（去掉测试侧 `store.upsert_fp_annotation`）。
- 回归网：calls 1-hop/signature/depth 警告；Focus Phase2 `ArgToParam` 无 summary；N5 + `focus_equivalence_vs_full_index`；FileLock 共享 reject。
- 死 `AnalysisNeeds` 变体已删；`contract_for` V1 路由全覆盖。
- BUG-6 fresh-call 陈旧窗口已关闭：`JobTracker` 同时保留 resume 所需的按 job built-files 历史与 project-wide 一次性刷新集合；`maybe_refresh_graph` 不依赖 `replay_focus_result`，每次都在 incremental batch 前经 `FocusRuntime` / `QueryRuntime` drain 后台写入。无需 listener 回调或跨请求 closure-id 状态。

强制测试矩阵见 [`testing.md`](./testing.md) §2.11。

## 10. 代码质量与技术债务清理

### 10.1 Capability Profile 数据声明

全部默认语言的 `LanguageCapabilityProfile` 经 `ProfileSpec` + `build_profile()` 声明构造。  
身份与一致性由 `test_<lang>_profile_identity` 及四项全局 profile 测试约束。

特殊能力：`scope_aware_binding` 已由产品路径证据覆盖 Python、TypeScript、
JavaScript、ArkTS、Java、C、C++、C#、Go、Rust、Kotlin、Cangjie、PHP 与 Ruby；
其中 Java 使用合法 sibling-block identity，ArkTS 仍受 TS grammar/ArkUI callback
边界约束。全部 14 种语言身份的 supported direct-variable mutation 已覆盖 direct
extraction、SQLite/Trace 与 cold Focus-vs-full-Index；previous value/explicit
RHS→aggregate Expr 为 0.75、Expr→coalesced Local 为 0.90，unsupported target 不得退化
为 RHS-only write、synthetic local 或 field store。Cangjie 还覆盖 direct simple
reassignment 的 RHS→Local 0.90 provenance 与 LHS read suppression。Java supported `if`-condition `instanceof` 与 arrow switch type/record
capture 已覆盖 scoped identity、0.75 aggregate provenance 和产品对拍，colon group、
其他 flow-sensitive boolean context、exact record projection 与 definite-assignment
仍保守；C# direct pattern capture 与 parenthesized nested designation 已覆盖 switch
arm identity；direct capture 使用 0.80 whole-subject flow，nested designation/property/list
capture 使用 0.72 aggregate subject flow。
C `include_resolution` / `function_pointer_tracking`、call_graph 0.65，C/C++/Go CFG
覆盖 direct same-function `Goto`；C# 另覆盖跨 finally/using cleanup 的有序 goto
退出，exact property/index/positional projection、definite-assignment 与 guard dependency
仍保守；C++
`include_resolution`。PHP `cfg` WithLimitations(0.60) 覆盖
branch/loop/switch/elseif、fall-through、numeric break/continue、direct same-function
`Goto`（含 finally continuation）与 terminal→Exit；parameter、assignment-created
local、`[]`/`list()` nested/keyed/by-reference destructuring target、foreach/catch/static
declaration 与 explicit anonymous-function capture 采用 file/function/method scope-chain
identity，key expression 保持读取，assignment whole RHS 与 foreach collection
保守流向各 target；direct file/function/method variable `op=` 与 prefix/postfix
`++`/`--` 保留 aggregate read-modify-write provenance，mutation Expr→Local 为 0.90。
PHP exact key/index projection、missing-key/null、reference-alias semantics、
dynamic/non-variable destructuring/mutation target、`??=` conditional execution、
prefix/postfix result timing、global alias、variable variable 与 arrow-function
ownership 仍保守，未显式捕获的
匿名函数外层 local 不越过 callable boundary。Ruby simple assignment、
flat/nested/rest multiple-assignment local target、parameter、rescue/for variable 与
case/in capture 使用 source-ordered method/module/class/block scope chain，block write
复用已存在的祖先 binding，新名字与 block parameter 保持 block-local，later outer
assignment 不追溯吞并 earlier block local；显式 RHS list 按顶层位置映射，single
aggregate RHS 与 nested/rest target 保留 group/slice 级保守流，结构投影、`to_ary`
coercion、implicit `nil` fill、parallel evaluation order 与 numbered parameter 不建模。
ArkTS `cfg` WithLimitations(0.55)、其余 TS-fallback dataflow 能力
WithLimitations(0.60)；Go 已覆盖普通 nested block identity、type-switch clause-local
alias/guard-value flow、same-block mixed `:=` 的 local/函数体参数复用、声明后激活、
switch/select clause 隔离与 blank identifier 抑制，以及 identifier-only select receive
的 `:=` clause-local declaration、`=` existing-binding reuse 与 0.78 aggregate flow；
case-type projection、function-literal ownership、exact receive-result component、
non-identifier receive target 与 parallel-assignment evaluation order 仍保守；Rust 已覆盖普通 nested block identity、arm-scoped match capture、
guard/body identity、scrutinee/guard-let fixed tuple/tuple-struct/struct/slice-prefix
projection chain 与 source-ordered activation；runtime-length suffix projection、borrow/move
mode 与 guard control dependency 仍保守；Kotlin 已覆盖 parameter/local/catch 的 scope-chain binding、
nested control-scope shadowing 与 typed late-declared local 的 concrete simple-assignment
origins；extension receiver、smart-cast 与编译器级 variable-initialization proof 仍保守；
Cangjie 已覆盖 parameter/simple-local nested block identity、arm-scoped match binding、
guard/body identity、branch/loop/`match` sibling CFG，以及 simple/nested-tuple/
enum-payload `for-in` 的 loop-scoped capture 与 iterable aggregate provenance；其他
tuple/destructuring、resource binding、exact iterator element/结构投影与 pattern
irrefutability 的编译器验证仍保守。当前 `FeatureOverride`
只保留有实际使用的 `WithLimitations` 变体。

**`atlas status` vs `doctor`**：status 只列**项目中有源文件**的语言；doctor 列全部编译语言（含无文件的 Cangjie 等）。

### 10.2 FeatureMatrix 镜像方法合并

✅ `FeatureMatrix` 现通过单一私有字段清单生成 supported/unsupported 名称并计算最低置信度，新增能力字段不再需要维护三套镜像列表。

### 10.3 设计味复核结论（2026-07）

对 `_investigation-atlas-full-pipeline-review.md` §6.3 设计味表的独立代码核验结论：

| 设计味 | 判决 | 代码事实 |
|--------|------|----------|
| **精度三词爆炸**（Mode/Mask/Precision/Level/GraphMode/IndexMode） | ✅ **已治理** | `architecture.md §1.1`（L29-33）已有 L0-L4 分层命名表；L21+L357 显式禁止再引入第二个 `IndexMode` 类型；`testing.md` L17 已同步。政策+类型层已解决。 |
| **DataflowFull 总档通胀**（旧状态把语言总档与逐 feature 能力混为一谈） | ✅ **已修复** | `CapabilityLevel::DataflowInterproc` 不再暗含 CFG；逐 feature 真值由 `FeatureMatrix` 给出。ArkTS CFG 为 WithLimitations(0.55)，PHP CFG 为 WithLimitations(0.60)，均由具体 fixture 约束。 |
| **LinuxAugment 双路径漂移**（index/lazy 路径分裂） | ✅ **已收敛** | `post_extract.rs` L1-6：Index 和 lazy structural 统一走 `extract_file_with_mode` -> `apply_post_extract_hooks`；3 个提取入口（extract.rs L201/L344/L769）共用。路径一致性已解决。 |
| **Schema V4 无迁移** | **已接受策略** | 架构 §6.1 明确"不保留旧 schema 运行时补丁路径"；`doctor` 存在；坏库 reject+重建指引已有。产品策略，非遗留。 |
| **Focus 塞 engine 源码树** | **真布局债（低优先级）** | `atlas-engine/src/focus/*` 仍在 engine 树内；`focus_materialize` 是唯一已 crate 化的 materialize 子 crate。布局随意但非正确性问题，长期可独立 crate。 |

**结论**：前 3 项已治理/修复/收敛，不应再列为债；Schema V4 是已接受策略；仅 Focus 布局为真债（低优先级）。

### 10.4 DEBT-3 god files 拆分（✅ 完成）

两个最大 god file 均已降至可维护规模，handler 全部按域隔离。

**`atlas-mcp/src/tools/mod.rs`：5,973 → 1,322 行**

- ✅ 3,372 行内嵌 `tools::tests` 机械迁移到 `mod_tests.rs`（94 个测试，模块身份不变）。
- ✅ 418 行 tool schema free fn 迁入 `tool_schemas.rs`（13 个 `make_*_tools` / `merge_edge_deps`）。
- ✅ 7 个 entry handler 迁入所属域模块：
  - `handle_calls` → `graph/calls.rs`（+ `CallsDispatch` / `resolve_calls_dispatch`）。
  - `handle_project` → `open_project.rs`。
  - `handle_symbol` + `handle_symbol_by_position` → `search.rs`。
  - `handle_fp_dispatches` → `annotations.rs`。
  - `handle_domain_rules` → `domain_rules.rs`。
  - `handle_tasks` → `atlas_jobs.rs`。
  - `handle_file_dependencies` 等 4 个 file-dep handler → `file_deps.rs`（376 行）。
- 残量 1,322 行 = 纯核心编排：`ToolRouter` struct + 构造 + prepare/refresh/ensure + `call_tool` + `dispatch_*`（8 变体）+ 共享 free fn（`node_json` / `get_str` / `validate_symbol_name_length` 等）+ `apply_focus_result_to_lr` / `known_gap_record`。

**`atlas-mcp/src/tools/graph.rs`：3,763 → 330 行**

- ✅ 1,544 行内嵌测试迁移到 `graph_tests.rs`（48 个测试，模块身份不变）。
- ✅ 4 个 handler 按依赖边界隔离到 `graph/` 子模块：
  - `graph/calls.rs`（706 行）：`handle_callers` / `handle_callees` / `handle_callgraph` + `CallsDispatch` + `handle_calls`。
  - `graph/path.rs`（643 行）：`handle_path` + path-only helpers。
  - `graph/explore.rs`（393 行）：`handle_explore` + `scoped_explore_resolution` + `parse_source_mode`。
  - `graph/impact.rs`（237 行）：`handle_impact` + `DEFAULT_IMPACT_EDGES`。
- 残量 330 行 = 纯共享 helper：symbol resolution、`parse_edge_kind`、`candidate_json`、`resolve_graph_symbol_with_focus_retry`、unresolved-call hint 等 path/calls/explore/impact 共用基础设施。

依赖方向单向（子模块 → 父共享 API，无反向依赖）。所有 `handler_purity` 测试持续绿色。

**后续**：无剩余 handler 拆分任务。`mod.rs` 核心编排（dispatch / prepare / refresh）是 `ToolRouter` 的固有职责，不属 god-file 债。

### 10.5 2026-08 架构复核后的剩余风险

- **CFG 语法策略仍过度集中**：共享 lowering 核心拥有 Entry/Exit、确定性 ID、
  continuation 和 path-isolated cleanup 是正确边界；但 `CfgLanguageConfig` 和多个
  `Language` 分支仍使新语言需修改 mega-builder。长期目标是把 grammar vocabulary
  和语言 policy 收回 per-language frontend CFG slot，共享核只保留图不变式；
  不新建 Function IR。该迁移必须以 14 语言 golden + persisted + Focus/Index
  parity 为前置，不做无验证的机械拆文件。
- **Linux post-extract 是项目语义，不是 C grammar**：当前它在唯一 extraction
  入口中统一运行，已消除 Index/Focus 漂移；但未来 Corpus 支持非 Linux C
  family 前，必须把 project-semantic augmentation 变为显式 caller policy/profile，
  不得把 Linux 规则偷渡成通用 blob extraction 语义。
- **Incremental sync 的正确性基线是 O(project files) hash scan**：这比相对 HEAD 的
  `git status` 更准确，能识别 clean checkout/pull/commit 后 DB 与磁盘不同。若实测证明
  成为瓶颈，只能引入可校验 inventory/mtime 快路径，最终仍以 raw content hash
  与 SQLite 为权威，不回退到 Git worktree status。
