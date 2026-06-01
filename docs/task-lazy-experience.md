# Atlas Lazy Analysis 体验改进 — 架构评估与分阶段路线图

> 状态: 用户评审通过，设计决策已收敛，可移交实现  
> 日期: 2026-06-01  
> 评阅人: software-architect  
> 复审: 用户确认（2026-06-01）  
> 依赖: 需求文档（用户提出的 12 点架构方向）

---

## 1. 现有架构评估

### 1.1 已有基础设施（强项）

Atlas 当前的 lazy 架构已经具备了提案中不少基础能力，不是从零开始：

| 提案方向 | 现有对应 | 成熟度 | 差距 |
|----------|---------|--------|------|
| Precision Model | `PrecisionTier` (6 级: Unavailable → Exact) | 已实现 | 只作用于 file/request 级别，未渗透到 per-symbol / per-result |
| Lazy 调度 | `LazyOrchestrator` + `LazyPolicy` 预设 | 已实现 | 策略只有 structural/dataflow 两层，无可组合的 capability-specific jobs |
| Partial 结果标注 | `LazyDiagnostics` + `next_action` | 已实现 | 只标注 "发生了什么"，未标注 "什么结论能下/不能下" |
| 后台 job 追踪 | `LazyCoordinator` + `extraction_jobs` 表 | 已实现 | 有 job tracking（queued→building→complete/failed），但 MCP 层未暴露 `query_id` / `resume` |
| 聚焦分析 | `LazyDataflowPlanner` (深度 2 调用图展开) | 已实现 | 仅基于调用图，不支持 field-level / branch-effect 级别的聚焦 |
| Budget 控制 | `LazyBudget` (时间 + 文件数双维度) | 已实现 | 只有 structural 和 dataflow 两种 budget，无 per-capability 预算 |
| CFG/Dataflow | `cfg_builder.rs` + `dataflow_builder.rs` | 已实现(9 语言) | 已有 intra-procedural dataflow + inter-procedural summary bridge |

### 1.2 关键缺口（提案正确定位的）

以下方向目前 Atlas **完全缺失**，且提案中定位准确：

1. **query_id / resume 机制** — 目前只有 atlass-cli 的 TUI 提示 `"run atlas index again to resume"`，MCP 层没有任何 "查询可恢复" 的概念。
2. **Investigation Graph** — 没有跨查询的调查上下文，每次 lazy 都是独立触发的。
3. **Capability-specific lazy jobs** — 只有 Manifest / Structural / LazyDataflow 三层，无法"只要 CFG 不要 dataflow"。
4. **Field-level lifecycle engine** — 不存在字段级生命周期模型，dataflow 仍是变量级。
5. **Branch effect diff** — 不存在 sibling branch 副作用比较。
6. **Domain rule layer** — 无 allocator/free 规则系统，无项目惯用法学习。
7. **Analysis contract** — 结果不说"我能证明什么/不能证明什么"。

### 1.3 提案中需要校正的部分

#### 校正 1: Precision Model 粒度 — 不要对每个 file/symbol 建立独立 precision 字段

提案建议对每个 file、symbol、edge、query result 添加独立的 precision metadata。这在理论上很完整，但会导致：
- 数据模型膨胀（每个表都要加 precision 列）
- 更新逻辑复杂（后台补全后需要级联更新大量行）
- 对 MCP agent 理解负担重（一堆字段）

**推荐替代方案**：保持现有 `extraction_state` 表作为 file-level precision 的单一入口（该表已有 `layer` 字段记录每文件的提取层级），新增 **capability 矩阵** 作为 `extraction_state` 的扩展列，而非扩散到所有实体表：

```sql
-- 扩展 extraction_state 表，而非修改 symbols/references 表
ALTER TABLE extraction_state ADD COLUMN capability_mask INTEGER DEFAULT 0;
-- bit 0: manifest, bit 1: ast, bit 2: refs, bit 3: calls, 
-- bit 4: cfg, bit 5: dataflow, bit 6: field_flow, bit 7: branch_effects
```

这样 precision 信息集中在一个表，不污染业务实体。

#### 校正 2: 不需要拆成"九层能力"，先从四层开始

提案中列出了 9 层能力（Manifest → AST → Semantic Edge → Function IR → Dataflow IR → Domain Models），但其中好几层在实现上存在强耦合：

- AST Index 和 Semantic Edge Index 依赖于同一趟 tree-sitter parse，拆开不会减少延迟。
- Function IR 如果包含了 CFG、def-use、calls、assignments，那它本身就需要 AST + CFG + dataflow 的结果。

**推荐替代方案**：从三到四层开始，对应实际可独立调度的提取阶段：

| Capability Layer | 对应现有 ExtractionMode | 可独立调度？ | 备注 |
|-----------------|------------------------|-------------|------|
| `manifest` | Manifest | 是 | 现有 |
| `structural` | Structural (symbols+refs+calls) | 是 | 现有 |
| `intra_cfg` | CFG only | 是（新） | 目前与 dataflow 耦合在 LazyDataflow 中 |
| `intra_dataflow` | CFG + dataflow | 是（新） | 目前与 CFG 耦合 |

`field_flow`、`branch_effects`、`ownership` 等更高级能力应该作为 **分析层** 构建在 `intra_dataflow` 之上，而非独立的 extraction layer。这样避免 extraction 层过度膨胀。

#### 校正 3: Investigation Graph 应当存于 SQLite，不应当纯内存

提案中的 investigation graph 如果只做内存结构，MCP server 重启就丢失。但也不应该设计成复杂的持久化查询上下文存储——那会引入状态管理复杂性。

**推荐方案**：
- Investigation 的核心状态（focus symbols, related files, desired capabilities）存入 `project_metadata` 或新表 `investigations`。
- 调度优先级和 job dependency 由 Scheduler 在内存中计算，不持久化。
- Investigation 的 TTL 应短（如 5 分钟无活动后过期），避免后台任务堆积。

#### 校正 4: 不要现在上 Function IR

提案中建议建轻量 Function IR 记录所有 parameters、locals、field_accesses、calls、assignments、frees、allocations、branches、exits、cfg_blocks。这个 IR 本质上是一个"简化版的程序表示"——维护两套表示（tree-sitter AST + Function IR）会导致同步问题。

**推荐替代方案**：
- CFG + dataflow 已经存在于 `cfg_nodes`/`cfg_edges`/`data_nodes`/`dataflow_edges` 表中。
- 新增的 field lifecycle / branch diff / ownership 分析应该直接消费这些已有表，而非建立中间 IR。
- 如果未来确实需要 IR，应该先评估是否可以直接扩展 `cfg_nodes` 表（加 `effect_kind` 列：Read/Write/Free/Allocate/Condition）而非建新表。
- **现阶段不要建 Function IR**，放到 Phase 3 做可行性评估后再决定。

#### 校正 5: "精度预算选择"应该推迟

提案的 `fast/focused/deep` 三档精度选择和 `precision` 参数，对 API 设计是合理的长期方向。但当前阶段更迫切的问题是：
- 用户看不到当前精度（提案第 1 点）
- 用户得不到针对性的深化（提案第 2 点）
- 用户不能 resume（提案第 3 点）

在解决这些问题之前，引入新的 precision 参数会增加 API 表面积而不会实质性改善体验。建议将 precision 选择放在 Phase 3 末尾。

#### 校正 6: 不要同时搞 "task_manager 的 job tracking" 和 "query recovery"

Atlas 已经在 `task_manager.rs` 中有后台任务管理，在 `lazy_coordinator.rs` 中有 extraction job tracking。`query_id` / `resume` 的设计需要明确区分：
- **Extraction job**（如 `parse lib/http.c`）：持久化到 `extraction_jobs` 表，id 为 job_id，可由 `task_status`/`wait_for_task` 查询。
- **Query result**（如 `q_abc123`）：指向一次 MCP tool call 的结果快照。因为是 MCP 层概念，不应复用 extraction job 的 id。

建议：`query_id` 是一个新的 MCP 层概念，它关联一个 `LazyWindow` 的快照 + 一个 `status: partial/ready` 标记。`atlas_resume(query_id)` 重新执行相同的 `LazyWindow` 计划并合并新结果。

---

## 2. 修正后的分阶段路线图

### Phase 1: Analysis Contract（2-3 周）

**目标**：让每个 MCP 返回都说清楚"能相信什么、不能相信什么"。

#### 1a. 扩展 Extraction State 为 Capability Mask

```rust
// crates/atlas-engine/crates/types/src/structs.rs (新增)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityMask(u16);
// bit definitions (仅包含 extraction 层能力):
// 0: manifest    — top-level symbols known
// 1: structural  — full symbol tree, scopes, references, callsites  
// 2: call_edges  — callsites resolved, call graph edges built
// 3: cfg         — per-function CFG built
// 4: dataflow    — intra-procedural dataflow built
// 5: summaries   — inter-procedural function summaries built
//
// 以下能力不属于 extraction mask，
// 而是由 analysis 层产出，体现在结果 evidence_level：
//   - field_lifecycle  (Phase 3)
//   - branch_diff      (Phase 3)
//   - ownership_proof  (Phase 4)
// 不要在 extraction_state.capability_mask 中为它们预留 bit。
```

Modify `extraction_state` table:
```sql
ALTER TABLE extraction_state ADD COLUMN capability_mask INTEGER NOT NULL DEFAULT 0;
```

Modify `LazyOutcome` 和 `LazyWindow` 返回中包含 per-file capability mask 摘要。

#### 1b. 在 MCP 响应中加入 Analysis Contract

扩展 `LazyDiagnostics` 为：

```rust
pub(crate) struct LazyDiagnostics {
    pub structural: Option<LazyLayerDiagnostics>,
    pub dataflow: Option<LazyLayerDiagnostics>,
    pub next_action: &'static str,
    // NEW
    pub analysis_contract: AnalysisContract,
}

pub(crate) struct AnalysisContract {
    /// What conclusions ARE supported by current data
    pub safe_conclusions: Vec<String>,       // e.g. "can list all AST-level references"
    /// What conclusions are NOT yet supported
    pub unsafe_conclusions: Vec<String>,     // e.g. "cannot prove path-sensitive ownership"
    /// Capabilities currently available per file (summarized)
    pub capability_summary: CapabilitySummary,
    /// What background jobs would improve this result
    pub refinement_jobs: Vec<RefinementJob>,
}
```

`AnalysisContract` 生成逻辑：
- `safe_conclusions` 由当前 capability mask 推导（如 mask 含 AST→"can confirm textual references"）
- `unsafe_conclusions` 由缺失的 capability bits 推导（如缺 dataflow→"cannot confirm dataflow completeness"）
- `refinement_jobs` 由 Investigation Graph 的 desired_capabilities - available_capabilities 差值生成

#### 1c. 修改现有 MCP 工具输出

将 `lazy_diagnostics` 中的 `next_action: "narrow_scope"` 和 `structural_hint: "budget exceeded..."` 替换为 `analysis_contract` 块。保留 `pending_job_ids` 以备 Phase 2 的 resume 使用。

**验证标准**：
- 任何触发 lazy 的 MCP 调用，返回 JSON 中必须有 `analysis_contract` 块。
- `safe_conclusions` 至少包含一条具体陈述（不能是空数组或泛泛的 "result may be incomplete"）。
- `unsafe_conclusions` 覆盖所有缺失的 capability（如缺 CFG → 必须说明 "cannot analyze branch-level control flow"）。

#### 1d. Per-result 精度标注

对于 `atlas_usages`、`atlas_neighbors` 这类返回多个结果的工具，在每个 `SymbolDef` / reference 条目上附加来源标注：

```json
{
  "symbol": "cookiehost",
  "evidence_level": "AST_REFERENCES",
  "source_capability": "structural"
}
```

这比全局 `PrecisionTier` 更精确，因为不同符号可能来自不同提取层级的文件。

**实现方式**：在 MCP handler 层根据返回符号的 `FileId` 查询 `extraction_state.capability_mask`，将 mask 映射为 `evidence_level` 字符串附加到输出。

---

### Phase 2: Query Recovery + Focused Scheduler（3-4 周）

**目标**：Partial 结果可以 resume，后台分析自动围绕用户调查目标深化。

#### 2a. query_id 与 atlas_resume

在 `ToolRouter` 中新增：

```rust
// 每个 MCP call 生成唯一 query_id (UUID v7 for time-sortable)
// 存入内存 HashMap<QueryId, QuerySnapshot>

struct QuerySnapshot {
    query_id: String,
    tool_name: String,       // "trace_variable", "usages" 等
    tool_args: Value,        // 原始参数，用于 resume 重跑
    lazy_window: LazyWindow, // 当时的 lazy plan
    created_at: Instant,
    status: QueryStatus,     // Partial | Refining | Ready
    last_result: Option<Value>,
}
```

新增 MCP tool: `atlas_resume(query_id: string) -> object`:
1. 查找 QuerySnapshot。
2. 重新 run LazyWindow 的 loader（跳过已 cached 的 units）。
3. 重新执行原始 tool handler（如 trace_variable）。
4. **返回完整增强结果**（不是 diff）。返回格式与原 tool 完全一致，只是 analysis_contract 中的 safe_conclusions 可能增加、refinement_jobs 可能减少。diff 模式留待未来，Phase 2 不引入。

TTL: QuerySnapshot 在 5 分钟无活动后过期（默认值，可通过 `LAZY_QUERY_TTL_SECS` 配置）。MCP server 重启后所有 snapshot 丢失（这是 trade-off，避免了持久化复杂性）。

#### 2b. Investigation Graph

在 `LazyOrchestrator` 中新增：

```rust
/// 按 Investigation 管理分析优先级
pub struct InvestigationState {
    /// 当前活跃的调查
    pub active_investigation: Option<Investigation>,
    /// investigation 的 TTL (默认 5 分钟，可通过 LAZY_INVESTIGATION_TTL_SECS 配置)
    last_activity: Instant,
}

pub struct Investigation {
    /// 调查焦点（用户最初查询的目标符号/字段）
    pub focus: InvestigationFocus,
    /// 相关符号（由调用图/引用图扩展得到）
    pub related_symbols: Vec<SymbolId>,
    /// 相关文件
    pub related_files: Vec<FileId>,
    /// 期望的分析能力
    pub desired_capabilities: CapabilityMask,
}

pub enum InvestigationFocus {
    Symbol(SymbolId),
    Field { struct_sym: SymbolId, field_path: String },
    Position { file_id: FileId, line: u32, col: u32 },
}
```

**Investigation 生命周期**：
1. 用户调用任何一个分析类 MCP tool（trace_variable, usages, callers, trace_forward 等）→ 自动创建或更新 Investigation。
2. 同一个 MCP session 内的后续查询，如果 target 与当前 Investigation 相关（同一文件、同一符号、相邻调用层），则复用并扩展 Investigation。
3. `atlas_resume` 的调用触发 Investigation 的后台 refinement。
4. 5 分钟无活动 → Investigation 过期，后台任务降级。

**关键设计决策**：Investigation 不是用户显式创建的，而是 MCP session 级别的隐式上下文。这避免了引入新的 API 概念（如 "create investigation" "close investigation"）。

#### 2c. Focused Lazy Scheduler

在 `LazyOrchestrator` / `LazyCoordinator` 中新增调度优先级：

```rust
/// 按 Investigation 相关性排序 lazy jobs
pub fn prioritize_jobs(
    jobs: &mut Vec<ExtractionJob>,
    investigation: Option<&Investigation>,
) {
    if let Some(inv) = investigation {
        jobs.sort_by_key(|job| {
            // 负分 = 高优先级
            let relevance = if inv.related_files.contains(&job.file_id) { -100 } else { 0 };
            let symbol_match = if inv.related_symbols.iter().any(|s| job.touches_symbol(s)) { -50 } else { 0 };
            let estimated_cost = job.estimated_cost_ms as i64;
            relevance + symbol_match + estimated_cost // 低成本优先
        });
    }
}
```

**行为变化**：
- 旧行为：lazy structural 按文件列表顺序处理，预算超了就停。
- 新行为：先排与当前 Investigation 相关的文件，再排其他文件。预算超了，至少 Investigation 相关的已经处理完。

#### 2d. 后台 Job 状态对用户可见

新增 MCP tool: `atlas_jobs(query_id: Option<string>) -> object`:
- 列出当前查询关联的后台 job（parsing, CFG building, dataflow building）。
- 如果有 `query_id`，过滤到该查询的 job。
- 每个 job 返回 `status`、`progress`、`estimated_remaining`。

这会替代目前的 `task_status` + `wait_for_task` 的模糊反馈。

**验证标准**：
- `atlas_resume` 对同一个 query_id 调用 3 次，结果精度应逐步提高（manifest → structural → dataflow）。
- Investigation 相关性排序：构建一个 300 文件的测试项目，查询某个深层函数，相关文件的 lazy 处理应排在无关文件之前。
- `atlas_jobs` 返回的 job 状态在 5 秒内反映实际后台进度。

---

### Phase 3: Function-Level Analysis Enhancement（3-4 周）

**目标**：在不建独立 Function IR 的前提下，提供 field lifecycle 和 branch diff 分析。

**初始语言范围**：Phase 3 的 CFG node effect annotation 和 field lifecycle/branch diff 分析 **先只支持 C/C++**。原因：(1) free/alloc/field lifecycle 在 C/C++ 中最有价值且逻辑最明确；(2) 一次性改 9 个语言的 CFG builder 会将 Phase 3 风险放大。其他语言在 C/C++ 方案验证后按需扩展。

#### 3a. CFG Node Effect Annotation

扩展 `cfg_nodes` 表：

```sql
ALTER TABLE cfg_nodes ADD COLUMN effect_kind TEXT;
-- 取值: read, write, allocate, free, call, condition, return, goto, assign
ALTER TABLE cfg_nodes ADD COLUMN target_field TEXT;
-- 如 "data->state.aptr.cookiehost"（对 struct field access 的规范化路径）
```

在 extraction 阶段（`cfg_builder.rs`），对每个 CFG node 标注其对哪些 field 做了什么操作。

**输入**：已有 tree-sitter AST + scope info + symbol resolution。
**输出**：带 effect annotation 的 CFG。
**成本评估**：约增加 10-15% extraction 时间，因为 effect annotation 是 O(n) 的 CFG node 遍历，不需要额外 dataflow 计算。

#### 3b. Field-Level Lifecycle State Machine

新增 analysis crate 模块 `crates/atlas-engine/crates/analysis/src/lifecycle.rs`：

```rust
/// 字段生命周期状态机
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldState {
    Unknown,
    MaybeLive,
    Assigned,     // x = aprintf(...)
    Freed,        // Safefree(x)
    Nullified,    // x = NULL
    Escaped,      // 传入外部函数
    Returned,     // return x
    Invalidated,  // 所在 struct 被 free
}

/// 字段生命周期引擎
pub struct FieldLifecycleEngine;

impl FieldLifecycleEngine {
    /// 对指定函数内的指定字段做路径敏感的 lifecycle 分析
    /// 输入: 带 effect annotation 的 CFG
    /// 输出: 每条路径出口的 field state + 可疑点列表
    pub fn analyze_field_lifecycle(
        cfg: &[CfgNode],
        field_path: &str,
        ownership_rules: &OwnershipRules,
    ) -> FieldLifecycleResult;
}
```

**状态转移规则**（简化版，仅覆盖 C/C++ owned pointer 字段）：
```
Safefree(x)     → x = Freed
x = alloc(...)   → x = Assigned  
x = NULL         → x = Nullified
x = escaped_ptr  → x = Escaped
use(x)           → 记录 use site (不改变状态)
return x         → exit snapshot: Returned
```

**不做的**：不处理指针算术、不处理 union、不支持跨函数 alias 分析——这些留给 Phase 4 Domain Model。

#### 3c. Branch Effect Diff

新增 `crates/atlas-engine/crates/analysis/src/branch_diff.rs`：

```rust
pub struct BranchDiffEngine;

impl BranchDiffEngine {
    /// 比较 if/else、switch case 的 sibling paths 的副作用差异
    pub fn diff_sibling_branches(
        cfg: &[CfgNode],
        function_symbol: &SymbolDef,
    ) -> Vec<BranchDiff>;
}

pub struct BranchDiff {
    pub branch_node: CfgNodeId,      // if/switch 语句
    pub common_prefix: String,        // 公共结构体前缀，如 "data->state.aptr"
    pub path_a: BranchPathSummary,
    pub path_b: BranchPathSummary,
    pub suspicious_asymmetry: Option<String>, // 不对称描述
}

pub struct BranchPathSummary {
    pub frees: Vec<String>,           // 释放的字段
    pub writes: Vec<String>,          // 写入的字段
    pub condition: Option<String>,    // 路径条件
}
```

**关键**：这不是传统 dataflow。它只比较两个分支在 CFG 上的 effect annotation 差异。O(n) time, no fixpoint iteration.

**验证标准**：
- 对 curl 的 `Curl_http` 函数运行 `analyze_field_lifecycle("data->state.aptr.cookiehost")`，应能在 100ms 内识别 `Safefree` → `aprintf` → 使用 的模式。
- 对已知的 if/else 不对称模式（一个分支 free 了某字段，另一个没 free），`diff_sibling_branches` 应产生一个 `suspicious_asymmetry` 条目。
- 不应产生误报：仅当确实存在结构体字段的读写不对称时，才报告。

#### 3d. 暴露为 MCP Tools

新增两个 MCP tools：

1. `atlas_lifecycle(symbol: string, field: string) -> object`:
   - 对指定函数内的指定字段做 lifecycle 分析。
   - 返回：字段状态机转移序列、每个路径出口的状态 snapshot、可疑点。
   - 如果 CFG 未就绪，触发 lazy CFG building。

2. `atlas_branch_diff(symbol: string) -> object`:
   - 对指定函数做分支副作用差异分析。
   - 返回：所有 sibling branch 的比较结果。
   - 如果 CFG 未就绪，触发 lazy CFG building。

---

### Phase 4: Domain Rules + Lifecycle Proof（4-6 周）

**目标**：让 Atlas 理解项目惯用法，将"导航证据"升级为"生命周期证明"。

#### 4a. Domain Rule System

新增 `crates/atlas-engine/crates/domain_rules/` crate：

```rust
/// 所有权规则定义
pub struct OwnershipRules {
    pub free_functions: Vec<String>,       // "free", "Curl_safefree", "Safefree"
    pub allocation_functions: Vec<String>, // "malloc", "aprintf", "strdup"
    pub owned_field_patterns: Vec<String>, // "data->state.aptr.*"
    pub cleanup_functions: Vec<String>,    // "Curl_freeset"
}

/// 规则来源
pub enum RuleSource {
    Builtin,           // 内置 C/C++ 通用规则
    ProjectLearned,    // 从项目中自动学习（见 4b）
    UserAnnotation,    // 用户显式标注（见 4c）
}
```

规则存储：SQLite 新表 `domain_rules`：
```sql
CREATE TABLE domain_rules (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,     -- builtin / learned / user
    rule_kind TEXT NOT NULL,  -- free_fn / alloc_fn / owned_pattern / cleanup_fn
    pattern TEXT NOT NULL,    -- 函数名或字段模式
    confidence REAL DEFAULT 1.0,
    created_at TEXT NOT NULL
);
```

#### 4b. 自动规则学习

在 `atlas index` 完成后运行一次静态分析：

1. 扫描所有 `dataflow_edges`，找到调用 `free()` / 已知 free 函数之后紧邻的赋值模式。
2. 统计：哪些函数被大量用于释放 struct 字段 → 候选 free function。
3. 统计：哪些函数返回值被大量赋值给 struct 字段 → 候选 allocation function。
4. 置信度阈值：至少 5 个独立使用点，且 >80% 的使用与模式一致，才自动加入 learned rules。

**注意**：Learned rules 默认不生效，需要用户通过 `atlas domain_rules list` 审查后 approve。避免自动化引入噪音。

#### 4c. 用户 Annotation 支持

新增 MCP tool: `atlas annotate(rule_kind: string, pattern: string) -> object`

示例：
```json
{
  "tool": "annotate",
  "rule_kind": "owned_pattern",
  "pattern": "conn->data->state.*"
}
```

Annotation 持久化到 `domain_rules` 表，source=`user`，在后续所有分析中生效。

#### 4d. Lifecycle Proof Mode

扩展 `FieldLifecycleEngine` 支持带 domain rules 的证明模式：

```rust
pub struct LifecycleProof {
    pub field_path: String,
    pub function: String,
    pub paths: Vec<PathProof>,
    pub verdict: LifecycleVerdict,
}

pub enum LifecycleVerdict {
    Safe,                    // 所有路径正确处理
    Suspicious(Suspicion),   // 存在可疑点
    Incomplete(String),      // 缺 dataflow 无法证明
}

pub struct PathProof {
    pub conditions: Vec<String>,
    pub states: Vec<(String, FieldState)>,  // (行号, 状态)
    pub exit_state: FieldState,
}

pub struct Suspicion {
    pub description: String,
    pub asymmetric_paths: Vec<String>,
    pub evidence_level: EvidenceLevel,
}
```

当 domain rules 覆盖了相关 free/alloc 函数后，lifecycle 分析可以从 "pattern observation" 升级为 "rule-backed proof"。`evidence_level` 变为 `DOMAIN_RULE_BACKED`，confidence 提升。

**验证标准**：
- 对 curl 配置所有权规则后，`atlas_lifecycle("Curl_http", "data->state.aptr.cookiehost")` 的 verdict 应为 `Suspicious`。
- 对简单函数（单路径 free），verdict 应为 `Safe`。
- 规则在 `atlas index` 后自动学习出的 free functions 应与 curl 源码中的实际使用一致（准确率 >80%）。

---

### Phase 5: Impact 增强（2 周）

**目标**：区分 surface impact 和 semantic impact。

#### 5a. Semantic Impact Analysis

在 `atlas_impact` 工具中加入 semantic impact 分析：

```json
{
  "surface_impact": {
    "direct_callers": ["Curl_http", "multi_do"],
    "direct_callees": ["Safefree", "aprintf"]
  },
  "semantic_impact": {
    "invariants_affected": [
      "dynamically_allocated_data.aptr ownership invariant",
      "host/cookiehost consistency invariant"
    ],
    "lifecycle_paths_affected": [
      "cookiehost cleanup in Curl_http",
      "host reassignment in Curl_close"
    ]
  }
}
```

Semantic impact 由以下方式计算：
1. 加载 domain rules（alloc/free/owned_pattern）。
2. 对 impact graph 中的每个函数，运行 lifecycle analysis。
3. 如果 lifecycle analysis 产出的 verdict 包含该字段，标记为 "invariant affected"。
4. 将结果分组为 invariant 级别的摘要。

---

## 3. 总体实施顺序与依赖

```
Phase 1 (Analysis Contract)  ← 零依赖，可立即开始
  ├── Phase 2 (Query Recovery + Focused Scheduler)
  │     ├── Phase 3 (Function-Level Analysis)
  │     │     ├── Phase 4 (Domain Rules + Lifecycle Proof)
  │     │     └── Phase 5 (Impact Enhancement)
```

- Phase 1 不依赖任何其他 Phase，且解决了最严重的用户体验问题（用户不知道结果可信度）。
- Phase 2 依赖 Phase 1 的 capability mask（需要知道当前缺什么才能调度）。
- Phase 3 依赖 Phase 2 的 focused scheduler（需要调度器先排相关函数的 CFG build）。
- Phase 4 依赖 Phase 3 的 field lifecycle engine（domain rules 是 lifecycle 的强化，而非前置）。
- Phase 5 依赖 Phase 3 和 Phase 4（semantic impact 需要 lifecycle analysis + domain rules 才能产出有意义的结果）。

---

## 4. 不做的事（明确排除）

1. **不建独立的 Function IR**。当前 `cfg_nodes` + `data_nodes` + `dataflow_edges` 已经构成足够分析的基础。在 Phase 3 发现有不可逾越的性能或表达能力障碍时，再重新评估是否需要 IR。
2. **不上 per-file/per-symbol 的 precision 字段污染所有实体表**。使用 `extraction_state.capability_mask` 作为集中入口。
3. **不引入用户可见的 "create/close investigation" API**。Investigation 是 MCP session 级别的隐式上下文。
4. **Phase 1 不上 precision 选择参数**（fast/focused/deep）。优先让用户理解当前精度，再让用户选择精度。
5. **不建完整的跨函数 dataflow 全量分析**。对 C/C++ 项目，这是非常高的投入。先用 field lifecycle + branch diff 覆盖 80% 的漏洞分析场景。

---

## 5. 向 Coder 移交

当用户确认本路线图后，请切换到 `coder` agent 执行实现。各 Phase 的实现顺序和验收标准见上文。

**实现前 coder 必须阅读**：
- `crates/atlas-engine/crates/types/src/structs.rs` — PrecisionTier 定义
- `crates/atlas-engine/src/lazy_orchestrator.rs` — LazyOrchestrator API
- `crates/atlas-engine/src/lazy_coordinator.rs` — Job tracking + ClosurePlanner
- `crates/atlas-engine/crates/lazy/src/planner.rs` — LazyDataflowPlanner
- `crates/atlas-engine/crates/lazy/src/loader.rs` — LazyDataflowLoader
- `crates/atlas-mcp/src/tools/lazy_response.rs` — LazyDiagnostics
- `crates/atlas-mcp/src/tools/trace.rs` — Trace handler (现有 precision 集成参考)

**禁止越界的模块边界**：
- Phase 1 只在 types + extraction + MCP 层改动，不碰 analysis/graph 层。
- Phase 3 的 field lifecycle 只依赖 `cfg_nodes` + `cfg_edges` + `dataflow_edges`，不新建中间表示层。
- domain_rules crate 只被 analysis crate 依赖，不反向依赖 extraction。

**已确认的设计决策**（2026-06-01 用户评审通过）：

1. **Investigation TTL**: 默认 5 分钟，通过 `LAZY_INVESTIGATION_TTL_SECS` 配置化。
2. **自动规则学习**: Phase 4 的 learned rules 默认不生效，需要用户通过 `atlas domain_rules approve` 审查后激活。这是安全默认。
3. **CFG effect annotation**: Phase 3 先只做 C/C++，不铺开到全部 9 个支持 CFG 的语言。其他语言在 C/C++ 方案验证后按需扩展。
4. **`atlas_resume` 返回格式**: 返回完整增强结果（与原 tool 格式一致），不做 diff。diff 模式留待未来。
