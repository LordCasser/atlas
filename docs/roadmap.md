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
- Freeze V1 tool schemas and document argument requirements.
- Add machine-readable version metadata for MCP clients.
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

`fp_dispatches` tool can map a struct's data-typed field (e.g., `rtnl_link_ops.kind`) to a target function via user annotation, but **function-pointer fields** (e.g., `rtnl_link_ops.changelink`, `.newlink`, `.doit`) are not individually indexed as symbols. Without a per-field symbol, `fp_dispatches` cannot declare `changelink → ipip6_changelink`. This blocks the only escape hatch for function-pointer call graph boundaries — trace/path queries stop at indirect calls.

**Why**: Kernel code (and C generally) uses function-pointer tables pervasively (e.g., `rtnl_link_ops`, `proto_ops`, `file_operations`). Today, `atlas_trace(forward)` cannot find `ipip6_changelink → ns_capable` because the `changelink` dispatch is opaque. Annotations that map `struct.field → target_fn` would bridge this gap: the tracer walks `rtnl_link_ops` callers, finds the annotated `changelink` field, and follows the declared target.

**Effect**: Unlocks cross-function-pointer trace, path, and call-graph queries for all struct-based dispatch patterns in the Linux kernel (and C generally). Currently the #1 precision ceiling for kernel vulnerability analysis.

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

Lazy structural extraction has a budget cap (~18s / 30 files for foreground, ~60s / 100 files for background). Very large source files (>2000-line functions like `copy_user_syms` in `kernel/trace/bpf_trace.c`) can exhaust this budget before completing structural extraction, causing tools (`calls`, `trace`, `explore`) to time out on those symbols. The file then stays "building" across retries without converging.

**Why**: Linux kernel has ~70 files with >10,000 lines and individual functions exceeding 2,000 lines. When an agent queries a symbol in one of these files, the lazy window processes the entire file (not just the target function). Tree-sitter parse + SCM query + dataflow/CFG build for a single huge file can independently exceed the per-window time budget, even when the file is the only unit in the window.

**Effect**: Makes `calls`, `trace(forward)`, and `explore` reliably available for all kernel symbols regardless of file size. Currently ~5-10% of kernel functions are unreachable through lazy extraction on first query.

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
- **Switch statement sibling detection**: `branch_diff` currently returns `branch_count=0` for functions with `switch` statements because CFG models each `case` as a linear successor to the dispatch node, not as parallel sibling branches. A function like `filter_pred_fn_call` (which selects among string/call/function/etc predicates via switch) is the exact shape where branch_diff is most valuable — asymmetric cleanup or locking per case. Extend CfgBuilder to emit sibling edges between switch cases so BranchDiffEngine can compare them.
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
- **Include-header structs in focus closure**: Currently `atlas_explore` on header-only structs like `dst_entry` (defined in `include/net/dst.h`) returns "building" because the focus closure only expands along call-graph and import-neighborhood edges from the seed symbol. A struct defined in a header that is `#include`-d by the seed's file is a direct import dependency, and its extraction cost is bounded (single struct body, trivial dataflow). Without this, agents analyzing kernel code cannot inspect the definition of any struct whose defining header hasn't been separately queried.

### 9.4 不变边界

`LazyStructuralService`、`LazyDataflowService`、`ExtractionMode`、`extraction_state` 和
`extraction_jobs` 保留为事实构建、缓存、freshness、in-flight dedup 边界。Focus 只替换
查询时的调度和决策层，不重写 extraction 管线。

详见 [`architecture.md` §10.1.10-10.1.11](./architecture.md) 中的 Focus-Lazy 架构约束。

## 10. 代码质量与技术债务清理

### 10.1 Capability Profile 数据声明化

当前 `types/src/capability.rs` 中 14 种语言的 `LanguageCapabilityProfile` 均以 ~60-70 行的 struct literal 硬编码构造，约 80% 字段为重复样板。Go 和 Python 已通过 `ProfileSpec` + `build_profile()` 原型迁移为数据声明模式。

- 将剩余 12 种语言（TypeScript、JavaScript、Java、C、C++、C#、PHP、Ruby、Rust、Kotlin、Cangjie、ArkTS）逐语言迁移到 `ProfileSpec` 模式。
- 每个迁移以 identity test 验证产出 `LanguageCapabilityProfile` 与迁移前完全一致。
- 迁移完成后考虑删除原始 `ts_profile()` / `java_profile()` 等构造器，统一为 `build_profile(&LANG_PROFILE_SPEC)`。

### 10.2 FeatureMatrix 镜像方法合并

✅ `FeatureMatrix` 现通过单一私有字段清单生成 supported/unsupported 名称并计算最低置信度，新增能力字段不再需要维护三套镜像列表。
