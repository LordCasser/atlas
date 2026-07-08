# Atlas Roadmap

This roadmap tracks **current and future work only**.

## 1. Current release focus: Atlas 1.5.x

Goal: ship a stable first version where CLI and MCP tools are usable by end users and agents against a local repository.

### 1.1 Packaging and installation

- Publish or document a repeatable release build flow for macOS, Linux, and Windows.
- Document verified platform matrix, minimum Rust version, and feature choices.
- Decide whether releases are distributed as source-only, release binaries, or both.
- Add release notes / changelog entry for the first public version.

### 1.2 User-facing documentation

- Keep `README.md` as the primary user entry point: installation, quickstart, CLI, MCP, architecture, language support, limitations.
- Keep `docs/trace-contract.md` as the stable reference for trace JSON output.
- Keep `docs/architecture.md` as the single authoritative architecture document.

### 1.3 MCP production hardening

- Freeze V1 MCP tool naming: short names without `atlas_` prefix. ✅ Done.
- Freeze V1 tool schemas and document argument requirements. ✅ Done: tool
  names, schema property sets, and required fields are locked by
  `schema_validation`; README documents primary required arguments.
- Add machine-readable version metadata for MCP clients. ✅ Done: `project(status)`
  returns `server.atlas_version`, `server.tool_contract_version`, and
  `server.compiled_features`, with regression coverage.
- Finalize graph snapshot refresh semantics.
- Keep all MCP outputs bounded and ensure truncation is visible in the response.

### 1.4 CLI and database release gates

- Ensure `atlas doctor` exposes release-relevant state.
- Compatibility: Schema V2 with no migration chain for older development schemas (direct DDL changes + re-index).
- Make `.atlas/` cleanup and rebuild guidance explicit.
- Keep JSON output stable for scripted use.
- Publish verified performance baselines.

### 1.5 Release smoke tests

```bash
cargo test
cargo test -p atlas-cli --features mcp
cargo check -p atlas-cli --features mcp
```

### 1.6 Completed baseline release gates

The original baseline implementation blockers are closed and covered by the release test matrix:

- ✅ Workspace and MCP feature test suites compile and pass against the current schema and types.
- ✅ Shared `run_index_pipeline` owns deleted-file cleanup and persistent summary construction.
- ✅ Summary capability is persisted only after summary construction succeeds.
- ✅ Every language has an explicit top-level-only Manifest path.
- ✅ CLI rejects unknown `--analysis` values.
- ✅ Lazy-triggering MCP tools use the shared public analysis view, including no-result trace and CFG-consuming paths.

## 2. Completed work

### 2.1 DataflowFull + persistent summary layer ✅

All 14 languages are now at `DataflowFull` level. The current schema added 4 persistent summary tables (`function_summaries`, `summary_param_reaches`, `summary_return_sources`, `summary_call_arg_sources`) with `CrossFunctionBridge` for ArgToParam/ReturnToCall interprocedural bridges.

> **CFG status (updated 2026-06)**: CFG builder (`cfg_builder.rs`) traverses branch/loop bodies for 12 capability-enabled languages; ArkTS and PHP remain unsupported. Golden fixtures cover core branch/loop behavior and language-specific resource constructs, including C#, Ruby, Kotlin, and Cangjie in addition to the original TypeScript/Python/Java/C/C++/Go/Rust set.

### 2.2 Lazy Index (three phases) ✅

- **P0: Scope Index** — `--include`/`--scope`/`--exclude` range-limited indexing.
- **P1: Manifest Extraction** — `ExtractionMode::Manifest` for lightweight top-level symbol extraction.
- **P2: Lazy Structural** — on-demand structural extraction via `LazyStructuralService`.

### 2.3 Workspace/crate split ✅

Project split into `atlas-engine` facade, engine internal crates, `atlas-mcp`, and `atlas-cli`.

### 2.4 Performance optimizations ✅

P0-P7 optimizations completed: PhaseTimings, hash-based dirty-set, thread-local parsers, batch DB writes, GlobalSymbolIndex, Rayon parallel edges, on-demand dataflow/CFG, language-capability-driven skip.

### 2.5 Lazy UX and query recovery ✅

- `CapabilityMask` centralizes extraction-layer capability state (`manifest`, `structural`, `call_edges`, `cfg`, `dataflow`, `summaries`) in `extraction_state`.
- Lazy MCP responses expose one shared public view: `analysis`, structured `gaps`, `query_id`, and resumable refinement state.
- MCP query snapshots support `resume_query(query_id)` for in-session recovery; snapshots are intentionally in-memory with a short TTL.
- Investigation state tracks the active MCP-session focus and desired capabilities for focused lazy refinement.
- `tasks` exposes query-related lazy/background job state.

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
- Multiline type ranges across supported brace-based languages participate in the same
  stale-cache invariant and one-time self-healing path.
- TUI native search is independent of graph snapshot readiness; first detail loading and
  stale graph refresh run through the background job system.

## 3. Trace and language capability work

### 3.1 Capability alignment

- Keep `language_capabilities` and `atlas doctor` aligned with actual compiled features.
- For each language, maintain explicit limitations and confidence floors.
- Ensure unsupported or partial trace queries return diagnostics rather than silent empty results.
- Keep `CapabilityMask` synchronized with persisted state: `cfg` requires actual CFG facts, `dataflow` requires dataflow facts, and `summaries` requires successfully built summary tables.
- `analysis.basis` may only advertise facts proven by DB state or verified during the current tool call.

### 3.2 Path-level validation

Continue expanding end-to-end smoke tests for all languages.

- Add per-language Manifest validation fixtures that include both top-level and local declarations.
- Add shared-pipeline parity tests for Manifest, Structural, and Full against CLI index/sync behavior.
- Add lazy dataflow tests for build, cache hit, full-index prebuilt cache, pending, partial, no-path trace, and CFG-consuming tool paths.

### 3.3 Public analysis view consistency

- Keep all lazy-triggering MCP tools aligned on `analysis`, structured `gaps`, and terminal retry semantics.
- Keep internal `CapabilityMask` details behind the public `analysis.basis` and `gaps[].reason` boundary.
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

`atlas-engine` already exists as a facade crate. The next step is API stabilization:

- Define the minimal supported public API surface.
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
- Capability profiles currently expose CFG for 12 languages; ArkTS and PHP remain the exceptions.

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
| Phase 3 | ClosureEngine | 策略驱动的有限不动点闭包扩展（ImportNeighborhood/CallGraph/TypeGraph），含预算控制 |
| Phase 4 | ScopedResolver + FocusGraphBuilder | 闭包作用域引用解析和 scoped graph overlay |
| Phase 5 | MCP Response Envelope 统一 | `analysis`/`coverage_counts`/`gaps`/`query_id` 统一 public view，删除 `precision`/`work` 等伪信号 |
| Phase 6 | 旧控制平面清理 | `LazyOrchestrator`/`LazyCoordinator` 已从模块系统移除，MCP 不再使用 `ensure_structural_*` |
| Phase 7 | 冷启动闭包正确性 | 精确 symbol frontier、dependency resolution-only、深度驱动 fixed point、后台 materialization refresh、成功/失败终态、完整 C/C++ type ranges 和旧 type-range cache 自愈 |

### 9.3 剩余工作

- 长期：继续收敛 extraction/focus 内部 precision 类型，保持 MCP 公共边界稳定且最小。
- 长期：以真实大型仓库 smoke 和受控 fixtures 持续测量 cold incoming candidate discovery；
  只有测量证明现有 bounded provider 不足时才引入新的索引实体。
- **Include-header structs in focus closure** — *foreground/background path implemented*: request-scoped `include_roots` thread from the MCP tool boundary through `prepare_focus_query_with_roots` → `QueryRuntime::prepare` → `FocusRuntime::prepare`, then are copied onto each `FocusWindow`. `ClosureEngine` no longer stores mutable per-query roots; `materialize_import_dependencies` reads roots from the window, so foreground closures, scheduled background closures, and hot-region extension windows all use the roots of the query that created them. Non-request prewarming still carries an empty roots vector by design.
  - **Remaining validation:** keep C/C++ angle-include fixtures and large-repo smoke coverage for `search`, `symbol(detail/usages)`, `context`, `calls`, `explore`, `trace`, `path`, `lifecycle`, and `branch_diff`; avoid reintroducing mutable include roots on cached engines.

### 9.4 不变边界

`LazyStructuralService`、`LazyDataflowService`、`ExtractionMode`、`extraction_state` 和
`extraction_jobs` 保留为事实构建、缓存、freshness、in-flight dedup 边界。Focus 只替换
查询时的调度和决策层，不重写 extraction 管线。

详见 [`architecture.md` §10.1.10-10.1.11](./architecture.md) 中的 Focus-Lazy 架构约束。

## 10. 代码质量与技术债务清理

### 10.1 Capability Profile 数据声明化

✅ 全部 14 种语言的 `LanguageCapabilityProfile` 现已统一通过 `ProfileSpec` + `build_profile()` 数据声明模式构造。此前各语言以 ~60-70 行 struct literal 硬编码，约 80% 字段为重复样板。

- 剩余 12 种语言（TypeScript、JavaScript、Java、C、C++、C#、PHP、Ruby、Rust、Kotlin、Cangjie、ArkTS）已在 Go/Python 原型之后完成迁移。
- 每个迁移均以 per-language identity test（`test_<lang>_profile_identity`）验证产出 `LanguageCapabilityProfile` 与迁移前逐字段完全一致；四项一致性测试（`test_all_profiles_are_valid`、`test_all_profiles_have_feature_matrix`、`test_cfg_feature_matrix_consistent_with_supported_features`、`test_cfg_known_limitation`）保持通过。
- 特殊情形保留：C 的 `include_resolution`+`function_pointer_tracking` 及 call_graph 0.65；C++ 的 `include_resolution`；ArkTS/PHP 的 `cfg` 为 Unsupported；Cangjie 由 `fm.supported_feature_names()` 派生改写为显式列表（13 项全支持、unsupported 为空）；C#/Ruby 的 CFG limitation 文本含 "body traversal"+"implemented"。现有三个 `FeatureOverride` 变体（`Confidence`/`WithLimitations`/`Unsupported`）足以表达全部覆盖，未新增变体。

**Note — `atlas status` 的语言列表语义（避免误判为 Cangjie 缺陷）**：`atlas status`（及 MCP `status`）按设计只列出**项目中实际存在源文件**的语言（遍历 `files_by_language`，见 `status.rs`）；`atlas doctor` 才用 `all_compiled()` 列出所有编译语言。因此若某项目无 `.cj` 文件，`status` 不显示 Cangjie 属正常语义，而非注册/profile 缺陷——`all_compiled()` 已含 Cangjie（`#[cfg(feature="cangjie")]`，默认启用），`atlas doctor` 正确显示 `cangjie dataflow_full 65%`。

### 10.2 FeatureMatrix 镜像方法合并

✅ `FeatureMatrix` 现通过单一私有字段清单生成 supported/unsupported 名称并计算最低置信度，新增能力字段不再需要维护三套镜像列表。
