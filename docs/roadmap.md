# Atlas Roadmap

Tracks **goals and remaining work**. Landed capabilities are stated in the present tense.  
Version-to-version changes belong only in [`CHANGELOG.md`](../CHANGELOG.md).

## 1. Current release focus: Atlas 1.5.x

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
- Add release notes / changelog entry for the first public version. ✅ Done:
  `CHANGELOG.md` contains 1.5.x release notes and an Unreleased release-hardening
  entry.

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
  ✅ Done: `ToolRouter::call_tool()` bounds returned text blocks to 25KB and
  emits an extra content block with truncation metadata; regression coverage
  verifies the marker on oversized tool output.

### 1.4 CLI and database release gates

- Ensure `atlas doctor` exposes release-relevant state. ✅ Done: doctor
  prints Atlas version, Schema V2 state, canonical index mode from `Store`, compiled
  features, and per-language capability profiles; helper tests cover schema and
  index-mode reads.
- Compatibility: Schema V2 with no migration chain for older development schemas
  (direct DDL changes + re-index). ✅ Done: `init_schema()` only initializes
  empty unversioned databases or current-version databases, stamps fresh DBs
  with `CURRENT_SCHEMA_VERSION`, and rejects non-empty v0 development databases
  with rebuild guidance.
- Make `.atlas/` cleanup and rebuild guidance explicit. ✅ Done: failing
  database/schema/index-mode checks print explicit `atlas index --project ...`
  rebuild guidance and `.atlas/atlas.db` cleanup instructions for incompatible
  development databases.
- Keep MCP and trace JSON output stable for scripted/agent use; CLI stdout JSON
  is not part of the current 1.5.x command surface. ✅ Done: engine trace
  envelope tests lock the serialized V1 fields, MCP schema validation freezes
  tool argument shapes, and `handler_regression` covers the `trace` tool through
  `ToolRouter::call_tool()` including `query_id`/`analysis` and retired-field
  exclusions.
- Publish verified performance baselines. ✅ Done: `docs/performance.md`
  includes the 2026-07-08 release-mode Atlas self-index smoke baseline on a
  clean `git archive HEAD` checkout, plus historical large-project baselines.

### 1.5 Release smoke tests

```bash
cargo test
cargo test -p atlas-cli --features mcp
cargo check -p atlas-cli --features mcp
```

✅ Done: verified on 2026-07-08. `cargo test --quiet`,
`cargo test -p atlas-cli --features mcp --quiet`, and
`cargo check -p atlas-cli --features mcp` all completed with exit code 0.

### 1.6 Completed baseline release gates

The original baseline implementation blockers are closed and covered by the release test matrix:

- ✅ Workspace and MCP feature test suites compile and pass against the current schema and types.
- ✅ Shared `run_index_pipeline` owns deleted-file cleanup and persistent summary construction.
- ✅ Summary capability is persisted only after summary construction succeeds.
- ✅ Every language has an explicit top-level-only Manifest path.
- ✅ CLI rejects unknown `--analysis` values.
- ✅ Lazy-triggering MCP tools use the shared public analysis view, including no-result trace and CFG-consuming paths.

## 2. Completed work

### 2.1 DataflowInterproc + persistent summary layer ✅

All 14 languages are now at `DataflowInterproc` level. The current schema added 4 persistent summary tables (`function_summaries`, `summary_param_reaches`, `summary_return_sources`, `summary_call_arg_sources`) with `CrossFunctionBridge` for ArgToParam/ReturnToCall interprocedural bridges.

> **CFG status (updated 2026-07)**: CFG builder (`cfg_builder.rs`) traverses branch/loop bodies for 13 capability-enabled languages; PHP remains unsupported. ArkTS named function/method CFG is enabled via TS grammar fallback with WithLimitations(0.55), verified by golden fixtures and trace tests. ArkUI trailing blocks collapse to statements and nested arrow callbacks do not have independent CFGs. Golden fixtures prove covered patterns, not compiler validity or a global confidence increase.

> **ArkTS state-flow status (updated 2026-07)**: `AppStorage.set/setOrCreate` incoming flow is query-time `StateFlow`, with exact `this`-field and literal/expression key-category matching. Full-cache and cold Focus paths are both covered; cold Focus uses `StateChannel` closure discovery plus writer-function dataflow materialization on resume. Reverse `StorageLink`, constant evaluation, timing, and process boundaries remain explicit limitations.

### 2.2 Index scope / manifest + Focus materialize

- **Scope Index** — `--include` / `--scope` / `--exclude`.
- **Manifest** — `ExtractionMode::Manifest` top-level symbols.
- **Focus materialize** — on-demand structural/dataflow under `FocusMaterialize`（机制类型可名 `Lazy*`；产品路径 Focus）。

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
- TUI native search is independent of graph snapshot readiness; first detail loading and
  stale graph refresh run through the background job system.

## 3. Trace and language capability work

### 3.1 Capability alignment

- Keep `language_capabilities` and `atlas doctor` aligned with actual compiled features.
- For each language, maintain explicit limitations and confidence floors.
- Ensure unsupported or partial trace queries return diagnostics rather than silent empty results.
- Keep `FactCoverage` synchronized with persisted state: `cfg` requires actual CFG facts, `dataflow` requires dataflow facts, and `summaries` requires successfully built summary tables.
- `analysis.basis` may only advertise facts proven by DB state or verified during the current tool call.

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
- Capability profiles currently expose CFG for 13 languages; PHP remains the exception.

### 8.2 Remaining precision work

- Expand golden/end-to-end fixtures for nested branches, switch/match, exceptional control flow, async boundaries, and language-specific resource constructs.
- **Switch statement sibling detection** — *Phase 1 implemented*: `CfgBuilder::walk_switch` now emits a `Branch` dispatch node with one `CfgEdgeKind::CaseBranch` edge per `case`/`default` clause into a shared `Join`, so `BranchDiffEngine` no longer returns `branch_count=0` for functions with `switch` statements. Both branch-diff engines (`branch_diff.rs` and `branch_diff_semantic.rs`) do N-way case comparison via an O(n) reference-union. Languages wired up: C/C++ (`switch_statement`→`case_statement`), Java (`switch_expression`→`switch_block`→`switch_block_statement_group`/`switch_rule`), JS/TS/ArkTS (`switch_statement`→`switch_body`→`switch_case`/`switch_default`), Go (`expression_switch_statement`/`type_switch_statement`→`expression_case`/`type_case`/`default_case`), C# (`switch_statement`→`switch_body`→`switch_section`). A function like `filter_pred_fn_call` (which selects among string/call/function/etc predicates via switch) is the exact shape where branch_diff is most valuable — asymmetric cleanup or locking per case.
  - **NOT handled (deliberately deferred to a later phase):**
    - **Fall-through semantics are not modeled.** Each case body is an independent path from the dispatch `Branch` to `Join`; a C-style `case` without a terminating `break` is treated as if it broke. Case tails only connect to `Join`, never to the next case, so the CFG is a safe under-approximation of inter-case flow (never a spurious inter-case edge).
    - **False-positive avoidance (contract: may under-report, must NOT over-report).** Because fall-through is invisible, both engines flag only the *all-but-one* shape — a resource freed/allocated in exactly `n-1` of the effectful cases with a single conspicuous gap — and require ≥ 3 effectful cases. Effect-less paths (empty fall-through labels, the synthetic no-match `Branch→Join` skip edge) are ignored, so a bare `case N:` fall-through can never be the flagged outlier. A resource touched by only one case is treated as an intentional special-case, not an asymmetry.
      - **Residual false positive — non-empty intentional fall-through.** The empty-body guard above only neutralizes *bare* fall-through labels. A case with a *non-empty* body that intentionally falls through to the next case (e.g. `case 1: log(); /* fall through */ case 2: free(x); break;`) is still modeled as two independent paths, so case 2 can look like the *unique freer* and case 1 like a conspicuous gap. This is a known residual outside the safe contract; it is rare in resource-cleanup code (which normally `break`s) and is accepted until fall-through edges are modeled.
    - **Lifecycle case-path context implemented.** `lifecycle.rs` now treats `CaseBranch` as a path-sensitive branch frame (`CasePath`) for real case/default bodies, while the synthetic `Branch→Join` no-match edge carries no frame. Normal edges into the switch `Join` still pop the case frame, so post-switch transitions do not inherit stale case context. Remaining lifecycle precision limits are the CFG-level fall-through model and cross-function boundaries, not branch-frame propagation.
    - Non-C-family `switch`-like constructs remain deferred as single statements: Rust `match_expression`, Python `match`, Kotlin `when`, Cangjie `match`, Ruby `case`/`when` (pattern-matching semantics with guards/bindings; their ASTs are not yet wired into `walk_switch`). `try_statement` is likewise still deferred.

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

特殊能力：C `include_resolution` / `function_pointer_tracking`、call_graph 0.65；C++ `include_resolution`；PHP `cfg` Unsupported；ArkTS `cfg` WithLimitations(0.55)、其余 TS-fallback dataflow 能力 WithLimitations(0.60)；Cangjie 全支持集；`FeatureOverride` 变体 `Confidence` | `WithLimitations` | `Unsupported`。

**`atlas status` vs `doctor`**：status 只列**项目中有源文件**的语言；doctor 列全部编译语言（含无文件的 Cangjie 等）。

### 10.2 FeatureMatrix 镜像方法合并

✅ `FeatureMatrix` 现通过单一私有字段清单生成 supported/unsupported 名称并计算最低置信度，新增能力字段不再需要维护三套镜像列表。

### 10.3 设计味复核结论（2026-07）

对 `_investigation-atlas-full-pipeline-review.md` §6.3 设计味表的独立代码核验结论：

| 设计味 | 判决 | 代码事实 |
|--------|------|----------|
| **精度三词爆炸**（Mode/Mask/Precision/Level/GraphMode/IndexMode） | ✅ **已治理** | `architecture.md §1.1`（L29-33）已有 L0-L4 分层命名表；L21+L357 显式禁止再引入第二个 `IndexMode` 类型；`testing.md` L17 已同步。政策+类型层已解决。 |
| **DataflowFull 总档通胀**（14 语言全 DataflowFull，ArkTS/PHP 无 CFG） | ✅ **已修复** | `capability.rs` L228 枚举为 `CapabilityLevel::DataflowInterproc`——无 `DataflowFull` 存在；ArkTS named function/method CFG 为 WithLimitations(0.55)，PHP（L1381）仍 Unsupported。§2.1 已使用 `DataflowInterproc` 新名。 |
| **LinuxAugment 双路径漂移**（index/lazy 路径分裂） | ✅ **已收敛** | `post_extract.rs` L1-6：Index 和 lazy structural 统一走 `extract_file_with_mode` -> `apply_post_extract_hooks`；3 个提取入口（extract.rs L201/L344/L769）共用。路径一致性已解决。 |
| **Schema V2 无迁移** | **已接受策略** | 架构 §6.1 明确"不保留旧 schema 运行时补丁路径"；`doctor` 存在；坏库 reject+重建指引已有。产品策略，非遗留。 |
| **Focus 塞 engine 源码树** | **真布局债（低优先级）** | `atlas-engine/src/focus/*` 仍在 engine 树内；`focus_materialize` 是唯一已 crate 化的 materialize 子 crate。布局随意但非正确性问题，长期可独立 crate。 |

**结论**：前 3 项已治理/修复/收敛，不应再列为债；Schema V2 是已接受策略；仅 Focus 布局为真债（低优先级）。

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
