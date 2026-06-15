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
cargo test -p atlas-cli --features mcp
cargo check -p atlas-cli --features mcp
```

### 1.6 V1 release blockers

These items must be closed before V1 is considered releasable:

- Fix any test compile failures caused by schema/type evolution; release validation cannot rely on a suite that does not compile.
- Make shared `run_index_pipeline` authoritative for full-index cleanup: deleted files must be removed from DB state when MCP or other shared-pipeline callers re-index.
- Make shared `run_index_pipeline(Full)` build persistent function summaries, matching CLI Full behavior.
- Persist `summaries` capability after successful summary build so user-visible `analysis_contract` can prove inter-procedural summary availability.
- Add explicit Manifest queries or explicit unsupported declarations for every language; Manifest must mean top-level symbols only.
- Validate CLI `--analysis` values instead of silently falling back to Structural.
- Align all lazy-triggering MCP tools on `analysis_contract`, including no-result trace responses and CFG-consuming tools.

## 2. Completed work

### 2.1 DataflowFull + persistent summary layer ✅

All 14 languages are now at `DataflowFull` level. The current schema added 4 persistent summary tables (`function_summaries`, `summary_param_reaches`, `summary_return_sources`, `summary_call_arg_sources`) with `CrossFunctionBridge` for ArgToParam/ReturnToCall interprocedural bridges.

> **CFG status (updated 2026-06)**: CFG builder (`cfg_builder.rs`) now fully traverses branch/loop bodies for all languages. Two language-specific wrapper-node issues (Go `statement_list`, Rust `expression_statement`) were fixed in the M2 CFG hardening milestone. All 9 languages with CFG support now produce complete control-flow graphs including statement nodes inside if/else branches and loop bodies. Golden fixtures cover TypeScript, Python, Go, Rust, Java, C, and C++ (cfg_if_else + cfg_loop).

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
- Lazy MCP responses expose `analysis_contract` with safe conclusions, unsafe conclusions, capability summary, and refinement jobs.
- MCP query snapshots support `resume_query(query_id)` for in-session recovery; snapshots are intentionally in-memory with a short TTL.
- Investigation state tracks the active MCP-session focus and desired capabilities for focused lazy refinement.
- `tasks` exposes query-related lazy/background job state.

### 2.6 Field lifecycle, branch diff, and semantic impact ✅

- C/C++-oriented `FieldLifecycleEngine` analyzes field state transitions from CFG/dataflow facts.
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

## 3. Trace and language capability work

### 3.1 Capability alignment

- Keep `language_capabilities` and `atlas doctor` aligned with actual compiled features.
- For each language, maintain explicit limitations and confidence floors.
- Ensure unsupported or partial trace queries return diagnostics rather than silent empty results.
- Keep `CapabilityMask` synchronized with persisted state: `cfg` requires actual CFG facts, `dataflow` requires dataflow facts, and `summaries` requires successfully built summary tables.
- `analysis_contract` may only advertise capabilities proven by DB state or by facts verified during the current tool call.

### 3.2 Path-level validation

Continue expanding end-to-end smoke tests for all languages.

- Add per-language Manifest validation fixtures that include both top-level and local declarations.
- Add shared-pipeline parity tests for Manifest, Structural, and Full against CLI index/sync behavior.
- Add lazy dataflow tests for build, cache hit, full-index prebuilt cache, pending, partial, no-path trace, and CFG-consuming tool paths.

### 3.3 Analysis contract consistency

- Keep all lazy-triggering MCP tools aligned on `analysis_contract`.
- Ensure `safe_conclusions` and `unsafe_conclusions` map directly to `CapabilityMask`.
- Keep `query_id`, `resume_query`, and `tasks` behavior documented and covered by tests.
- No MCP response may return a semantic/CFG result while its contract says that same capability is unavailable.
- No lazy-triggered query may omit `lazy_diagnostics` solely because the final trace/search result is empty.

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
- Decide which `atlas_`-prefixed analysis/domain-rules tools graduate into the stable short-name MCP surface.

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
- Full compiler-grade C/C++ ownership proof, pointer arithmetic, union aliasing, or complete cross-function dataflow.

## 8. Semantic analysis multi-language extension

### 8.1 branch_diff 架构演进与多语言扩展

#### 8.1.1 当前架构限制

`branch_diff` 和 `lifecycle` 当前存在架构层面的表达能力不足：

- **CFG 节点只挂单个粗粒度 effect**：`effect_kind + target_field` 模型无法表达一条语句包含多个语义 effect（如 alloc return + local assign + field store）
- **局部变量 ownership 中转不可追踪**：`alloc → local_var → field` 链路在当前模型下无法稳定追踪，导致类似 `ptr = malloc(); data->field = ptr;` 的模式漏报
- **branch_diff 是副作用集合 diff，不是 ownership/dataflow 分析**：缺少 value-flow IR 层，多层分支、switch/case、goto 存在漏遍历风险
- **仅 C/C++ 可用**：`analysis::ownership_rules.rs` 和 `domain_rules/engine.rs` 的核心基础设施已是语言无关的，但缺少其他语言的 rule consumer

#### 8.1.2 长期架构目标

从当前单层 CFG effect annotation 演进为分层语义分析管线：

```text
当前:  CFG node 上挂粗粒度 effect_kind + target_field
          ↓
目标:  CFG（控制流）→ DataFlow（值流）→ EffectIR（副作用语义）
       → Ownership Solver（生命周期推理）→ BranchDiff（语义查询视图）
```

五层管线说明：

| 层 | 职责 | 产出 |
|----|------|------|
| CFG | 控制流图（已有） | `cfg_nodes` / `cfg_edges` |
| DataFlow | 值流追踪（已有 `data_nodes`/`dataflow_edges`，需增强 alloc→local→field 链路） | per-variable use-def chains |
| EffectIR | 副作用语义建模（新增） | 每条语句的 multi-effect 列表，with provenance |
| Ownership Solver | 生命周期推理（新增） | field state machine（跨分支、跨函数） |
| BranchDiff | 基于 EffectIR 的分支语义对比（重构为查询视图） | 结构性不对称报告 |

#### 8.1.3 多语言分阶段扩展

**目标**：为以下语言添加 `LanguageOwnershipRules` consumer，使 `branch_diff`/`lifecycle`/`impact(semantic=true)` 能产生有意义的语义分析结果：

| 优先级 | 语言 | 关键模式 | 预计工作量 |
|--------|------|---------|-----------|
| P1 | Rust | `Box::new`/`Arc::new` 分配，`Drop` 释放，`unsafe` 边界 | 2-3d |
| P1 | Go | `make`/`new` 分配，`defer` + `Close()` 释放，goroutine ownership | 2-3d |
| P2 | Python | `open()` → `close()` 对，`with` 语句 RAII，`None` → 赋值 | 2d |
| P2 | TypeScript | `new` 分配，`Promise`/`async` 异步边界 | 2d |
| P3 | Java | `try-with-resources`，`close()` 模式 | 1-2d |
| P3 | C# | `IDisposable`/`using` 模式 | 1-2d |

**依赖项**：
- 目标语言需要 CFG 覆盖（见 §9.2 能力表）：Rust、Go 已有 CFG；Python/TypeScript 已有 CFG；Java/C# CFG 未实现
- `domain_rules` 需要每种语言的 builtin rules（alloc/free/owned pattern）
- `analysis` 层需要每种语言的 `OwnershipRules` trait 实现

**阶段划分**：
1. **Phase 1 (v1.4)**: Rust + Go — 语言已有 CFG，rule 模式明确
2. **Phase 2 (v1.5)**: Python + TypeScript — 高使用率语言，CFG 已覆盖
3. **Phase 3 (v2.0)**: 剩余语言随 CFG 实现一同交付

**不纳入范围**：SAST 级别的跨函数污点分析、完整 pointer provenance、编译器级 lifetime 验证。

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
| Phase 0 | Precision 替换 | MCP 响应已迁移到 `Precision { coverage, confidence }`；内部 `PrecisionTier` 在 MCP 边界转换 |
| Phase 1 | Bootstrap 冷启动 | `BootstrapManager`（Tier0 文件清单/Tier0.5 指纹/Tier1 SymbolHints/Tier2 机会性 manifest） |
| Phase 2 | FocusRuntime + QueryIntent | `QueryIntent → FocusRuntime::prepare` 统一入口；`QueryRuntime` 封装 MCP 集成 |
| Phase 3 | ClosureEngine | 策略驱动的有限不动点闭包扩展（ImportNeighborhood/CallGraph/TypeGraph），含预算控制 |
| Phase 4 | ScopedResolver + FocusGraphBuilder | 闭包作用域引用解析和 scoped graph overlay |
| Phase 5 | MCP Response Envelope 统一 | `analysis`/`precision`/`coverage_counts`/`gaps`/`work` 统一 envelope |
| Phase 6 | 旧控制平面清理 | `LazyOrchestrator`/`LazyCoordinator` 已从模块系统移除，MCP 不再使用 `ensure_structural_*` |

### 9.3 剩余工作

- 长期：将内部 `PrecisionTier` 统一迁移为 `PrecisionView`，消除 MCP 边界转换函数

### 9.4 不变边界

`LazyStructuralService`、`LazyDataflowService`、`ExtractionMode`、`extraction_state` 和
`extraction_jobs` 保留为事实构建、缓存、freshness、in-flight dedup 边界。Focus 只替换
查询时的调度和决策层，不重写 extraction 管线。

详见 [`architecture.md` §10.1.10-10.1.11](./architecture.md) 中的 Focus-Lazy 架构约束。
