# DataflowFull 架构设计

> **状态**: 架构已确认，待实现 | 创建: 2026-05-26

## 1. 问题定义

当前 13 种 `DataflowBasic` 语言需要演进到 `DataflowFull`。

### 1.1 DataflowFull 定义

`DataflowFull` 是 `CapabilityLevel` 的最高级别，定义为：

> 跨语句 scope-aware 的 use-def、跨函数 interprocedural flow、完整的 backward trace with access-path chains。

具体化为可检查的 `FeatureMatrix` 条件：

```
local_dataflow.is_supported()
  AND use_def.is_supported()
  AND interprocedural_summaries.is_supported()   ← 当前所有语言均为 Unsupported
  AND returns_flow.is_supported()
  AND call_arguments.is_supported()
```

### 1.2 核心设计决策：持久化摘要层

跨函数数据流桥接不通过 `dataflow_edges` 表扩张（如 `cross_function` 列），而是通过**独立的函数摘要层**。理由：

| 维度 | `dataflow_edges` 扩张 | 独立摘要层 |
|------|----------------------|-----------|
| 概念模型 | inter edges 混入 intra edges | 摘要 = 函数边界的闭包抽象 |
| 跨函数桥接 | 逐边遍历 | O(1) 摘要查询，复合沿调用图 |
| 增量失效 | 复杂 JOIN data_nodes | 直接 `DELETE WHERE function_id = ?` |
| 未来扩展 | 无 | Taint-like 查询、Impact v2、跨仓库分析 |

## 2. 摘要层架构

### 2.1 概念模型

```
dataflow_edges    = intra-procedural, fine-grained, direct edges (不变)
function_summary  = intra-procedural, transitive-closure, per-function (新增)
trace             = inter-procedural, by composing summaries along call graph
```

```
┌──────────────────────────────────────────────────┐
│              extraction (不变)                    │
│  DataFlowBuilder  →  dataflow_edges (intra only) │
│  SemanticBinder   →  bindings                    │
└──────────────────────┬───────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────┐
│           resolution + graph (不变)              │
│  ReferenceResolver → resolved references          │
│  GraphBuilder      → symbol_edges (call graph)   │
└──────────────────────┬───────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────┐
│         summary layer (NEW — analysis crate)     │
│                                                  │
│  SummaryBuilder (已有, BFS from params)          │
│  SummaryStore   (新增, 持久化 + 查询)            │
│  CrossFunctionBridge (新增, 替代 SummaryEdgeProvider) │
└──────────────────────┬───────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────┐
│              trace / slicer (不变)               │
│  Slicer → dataflow_edges + CrossFunctionBridge    │
│  TraceEngine → capability gating                 │
└──────────────────────────────────────────────────┘
```

### 2.2 持久化表设计 (Schema v3)

```sql
-- 函数摘要元数据，一行对应一个函数
CREATE TABLE function_summaries (
    function_id     BLOB PRIMARY KEY NOT NULL,
    node_count      INTEGER NOT NULL,
    edge_count      INTEGER NOT NULL,
    content_hash    TEXT NOT NULL,             -- 函数体源码 hash，增量失效 key
    computed_at     TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (function_id) REFERENCES symbols(symbol_id) ON DELETE CASCADE
);

-- 参数 P 的下游可达目标 T ("参数 → 调用参数/返回/字段")
CREATE TABLE summary_param_reaches (
    function_id     BLOB NOT NULL,
    param_id        BLOB NOT NULL,
    param_index     INTEGER NOT NULL,
    param_name      TEXT NOT NULL,
    target_kind     TEXT NOT NULL,             -- 'call_arg' | 'return' | 'field'
    target_node_id  BLOB NOT NULL,
    confidence      REAL NOT NULL DEFAULT 0.85,
    provenance      TEXT NOT NULL DEFAULT 'intraprocedural_dataflow',
    FOREIGN KEY (function_id) REFERENCES function_summaries(function_id) ON DELETE CASCADE
);
CREATE INDEX idx_spr_function ON summary_param_reaches(function_id);
CREATE INDEX idx_spr_param   ON summary_param_reaches(param_id);

-- 返回节点 R 的上游来源 S ("return ← 参数/局部变量")
CREATE TABLE summary_return_sources (
    function_id     BLOB NOT NULL,
    return_id       BLOB NOT NULL,
    source_node_id  BLOB NOT NULL,
    confidence      REAL NOT NULL DEFAULT 0.85,
    provenance      TEXT NOT NULL DEFAULT 'intraprocedural_dataflow',
    FOREIGN KEY (function_id) REFERENCES function_summaries(function_id) ON DELETE CASCADE
);
CREATE INDEX idx_srs_function ON summary_return_sources(function_id);
CREATE INDEX idx_srs_return   ON summary_return_sources(return_id);

-- 调用参数 A 的上游来源 S ("call_arg ← 参数/局部变量")
CREATE TABLE summary_call_arg_sources (
    function_id     BLOB NOT NULL,
    callsite_id     BLOB NOT NULL,
    arg_index       INTEGER NOT NULL,
    arg_node_id     BLOB NOT NULL,
    source_node_id  BLOB NOT NULL,
    confidence      REAL NOT NULL DEFAULT 0.85,
    provenance      TEXT NOT NULL DEFAULT 'intraprocedural_dataflow',
    FOREIGN KEY (function_id) REFERENCES function_summaries(function_id) ON DELETE CASCADE
);
CREATE INDEX idx_scas_function ON summary_call_arg_sources(function_id);
CREATE INDEX idx_scas_callsite ON summary_call_arg_sources(callsite_id);
```

### 2.3 摘要构建流程

```
SummaryBuilder::build(store, function_id) → FunctionSummary
    │
    ├── 1. 加载该函数的所有 DataNodes + DataFlowEdges
    ├── 2. 从每个 Parameter 节点 BFS 前向遍历 Reachable nodes
    │       → 记录 param_reaches (call_arg / return / field)
    ├── 3. 从每个 Return 节点 BFS 后向遍历 upstream nodes
    │       → 记录 return_sources
    ├── 4. 从每个 CallArg 节点 BFS 后向遍历 upstream nodes
    │       → 记录 call_arg_sources
    └── 5. 返回 FunctionSummary

SummaryStore::build_all(store)
    │
    ├── 遍历所有函数符号
    ├── 对每个函数调用 SummaryBuilder::build
    ├── 事务写入 4 张 summary 表
    └── 返回 build stats (functions, nodes, edges, elapsed)

SummaryStore::build_for_function(store, function_id)
    │  (lazy loading / incremental sync 时调用)
    ├── 先 invalidate_for_function(function_id)
    ├── SummaryBuilder::build(store, function_id)
    └── 事务写入 4 张 summary 表
```

### 2.4 计算时机策略

| 场景 | 策略 | 实现 |
|------|------|------|
| `atlas index`（全量） | 全局批处理 | resolution + graph 完成后，`SummaryStore::build_all(store)` |
| `atlas sync`（增量，文件少） | 逐文件增量 | 重新 extraction + resolution 后，`build_for_function` 受影响函数 |
| Lazy structural 加载 | 逐文件增量 | `StructuralLoader` 完成一个文件后，`build_for_function` 该文件内的函数 |

### 2.5 CrossFunctionBridge — 跨函数桥接

替代现有 `SummaryEdgeProvider` 的 runtime BFS：

```rust
/// 当 slicer 命中 Parameter 节点时，查找调用者的 call-arg → parameter 桥接
CrossFunctionBridge::incoming_for_param(param_id, store) → Vec<TraceEdge>
    ├── 通过 call graph 查找直接调用者 (callsites_by_callee)
    ├── 匹配 arg_index → parameter
    ├── 查询 summary_call_arg_sources，获取 call-arg 的上游来源
    │     → ArgToParam 虚边，confidence: 0.80 * source_confidence
    └── 返回所有桥接边

/// 当 slicer 命中 CallReturn 节点时，查找 callee 的 return → call-result 桥接
CrossFunctionBridge::incoming_for_call_result(call_result_id, store) → Vec<TraceEdge>
    ├── 通过 callsite_id 查找 callee
    ├── 查询 summary_return_sources，获取 callee 的 return 所有上游来源
    │     → ReturnToCall 虚边，confidence: 0.85 * source_confidence
    └── 返回所有桥接边
```

**向后兼容**：若 summary 表中无数据（旧 DB），`CrossFunctionBridge` 降级为当前 `SummaryEdgeProvider` 的 runtime BFS 逻辑。所有 trace 查询语义不变。

### 2.6 增量失效

```
sync(files F):
  1. 删除被重提取函数的所有 summary 行:
     DELETE FROM function_summaries  WHERE function_id IN affected;
     DELETE FROM summary_param_reaches WHERE function_id IN affected;
     DELETE FROM summary_return_sources WHERE function_id IN affected;
     DELETE FROM summary_call_arg_sources WHERE function_id IN affected;
  2. 对 affected 中的每个函数调用 build_for_function
  3. (Phase 3) 级联失效: 遍历受影响函数的调用者，标记其摘要为 stale
```

## 3. Capability Model 更新

### 3.1 derive_capability_level 更新

```rust
pub fn derive_capability_level(&self) -> CapabilityLevel {
    let has_dataflow = self.local_dataflow.is_supported()
        && self.use_def.is_supported();

    if has_dataflow
        && self.interprocedural_summaries.is_supported()
        && self.returns_flow.is_supported()
        && self.call_arguments.is_supported()
    {
        CapabilityLevel::DataflowFull
    } else if has_dataflow {
        CapabilityLevel::DataflowBasic
    } else if self.symbols.is_supported() && self.references.is_supported() {
        CapabilityLevel::Symbolic
    } else {
        CapabilityLevel::None
    }
}
```

### 3.2 每语言 DataflowFull 标准

语言标记为 `DataflowFull` 的前置条件：

1. Intra-procedural dataflow 完整（已有 `DataflowBasic` 标准）
2. `interprocedural_summaries: Supported`（摘要表有数据）
3. ≥1 个 golden fixture 验证跨函数 backward trace 包含 `ArgToParam` 和 `ReturnToCall` edge

### 3.3 语言推进顺序

```
Phase 2a (pilot):       TypeScript → JavaScript       confidence 0.55→0.60
Phase 2b (强静态):       Java → Go → C#               confidence 0.65→0.68 / 0.70→0.72
Phase 2c (中等难度):     Rust → PHP → Ruby → Kotlin   confidence 各自+0.02~0.05
Phase 2d (动态/难语言):   Python → C → C++ → ArkTS     confidence 各自+0.02~0.05
Phase 2e (保持):         Cangjie (Symbolic)
```

## 4. 实施计划

### Phase 1: 摘要存储层 + CrossFunctionBridge + Capability Model

**目标**：引擎层一次性完成，所有已编译语言可受益。

| 步骤 | 文件 | 改动 |
|------|------|------|
| 1a | `crates/atlas-engine/crates/db/src/schema.rs` | Schema v3: 4 张新表 + migration v2→v3 |
| 1b | `crates/atlas-engine/crates/db/src/store/summary.rs` | **新文件**: `SummaryStore`: CRUD + `build_all`/`build_for_function`/`invalidate_function` |
| 1c | `crates/atlas-engine/crates/db/src/store/mod.rs` | 注册 summary 子模块 |
| 1d | `crates/atlas-engine/crates/analysis/src/summary.rs` | Refactor: `SummaryBuilder` 保持纯计算逻辑，`SummaryStore` 负责持久化 |
| 1e | `crates/atlas-engine/crates/analysis/src/cross_function.rs` | **新文件**: `CrossFunctionBridge` |
| 1f | `crates/atlas-engine/crates/analysis/src/trace/virtual_edges.rs` | `SummaryEdgeProvider` → 委托给 `CrossFunctionBridge` |
| 1g | `crates/atlas-engine/crates/analysis/src/lib.rs` | 注册新模块 |
| 1h | `crates/atlas-engine/crates/types/src/capability.rs` | `derive_capability_level()` 新增 DataflowFull 分支 |
| 1i | `crates/atlas-cli/src/commands/index.rs` | `index` 最后调用 `SummaryStore::build_all` |
| 1j | `crates/atlas-cli/src/commands/sync.rs` | `sync` 最后调用 `build_for_function` 受影响函数 |
| 1k | 测试 | `schema.rs` 表存在性测试；`summary.rs` store 读写测试；`cross_function.rs` bridge 单元测试 |

### Phase 2a: TypeScript + JavaScript → DataflowFull (pilot)

| 步骤 | 文件 | 改动 |
|------|------|------|
| 2a.1 | `crates/atlas-engine/crates/types/src/capability.rs` | `ts_profile()`, `js_profile()`: `level → DataflowFull`, `interprocedural_summaries: Supported(0.55, [...])` |
| 2a.2 | `crates/atlas-cli/tests/trace_fixtures.rs` | 新增 `fx_ts_cross_function_arg_to_param` |
| 2a.3 | `crates/atlas-cli/tests/trace_fixtures.rs` | 新增 `fx_ts_cross_function_return_to_call` |
| 2a.4 | `crates/atlas-cli/tests/trace_e2e.rs` | 跨函数集成测试 |

### Phase 2b-2d: 其余语言 → DataflowFull

同上模式按语言逐个推进，每个语言至少一个跨函数 golden fixture。

### Phase 3: 增量失效优化（后续）

- 级联失效：函数变更时标记其调用者的摘要为 stale
- 按需重算：stale 摘要在下一次被查询时重建

## 5. 架构约束与不变式

1. **`dataflow_edges` 保持纯 intra-procedural。** 不新增 `cross_function` 或类似列。跨函数事实仅存在于摘要表。
2. **摘要是对 dataflow_edges 的闭包计算。** `SummaryBuilder` 输入 dataflow_edges，输出摘要。不依赖其他来源。
3. **Slicer 对摘要无感知。** 它只通过 `CrossFunctionBridge` 获取 virtual edges。持久化或 runtime 是实现细节。
4. **Resolution 必须在摘要构建前完成。** 摘要依赖 callsite → callee 的解析映射。
5. **新语言必须先有完整的 intra-procedural dataflow 才能构建摘要。** 不提供捷径。
6. **所有跨函数 trace 结果必须携带 confidence 分层：**
   - `0.85`: direct summary edge (intra-procedural BFS 可达)
   - `0.80 × 0.85 = 0.68`: single cross-boundary ArgToParam/ReturnToCall
   - `< 0.68`: multi-hop cross-boundary (confidence decay per hop)
7. **Cangjie 保持 `Symbolic`，不进入 DataflowFull 演进路径。**

## 6. 验证标准

### Phase 1 验证

```bash
# Schema 正确创建
cargo test -p atlas-db -- schema

# SummaryStore CRUD
cargo test -p atlas-db -- summary_store

# CrossFunctionBridge 单元测试
cargo test -p atlas-analysis -- cross_function

# derive_capability_level 返回 DataflowFull
cargo test -p atlas-types -- capability_level_roundtrip

# migrate 端到端 (v2→v3)
cargo test -p atlas-db -- migrations
```

### Phase 2 验证 (以 TS 为例)

```bash
# capability profile 声明正确
cargo test -p atlas-types -- ts_feature_matrix

# 跨函数 golden fixture
cargo test --test trace_fixtures --features typescript

# CLI/MCP trace 输出 dataflow_full
atlas trace variable --file cross_fn.ts --line 10 --column 5 --json \
  | jq '.capability.capability_level'  # "dataflow_full"
```

### 不可验证即未完成

- 若某语言的 golden fixture 不能通过 → 该语言不得升级到 DataflowFull
- 若 `derive_capability_level()` 测试失败 → Phase 1 未完成
- 若 CLI `capability_level` 仍为 `dataflow_basic` → profile 未生效

## 7. 移交 Coder

### 实现前必须读取的文件

| 文件 | 原因 |
|------|------|
| `docs/09-dataflow-full-architecture.md` | 本文档 — 架构约束 |
| `docs/02-architecture-constraints.md` | 不变式 §5 (fact 模型), §6 (语言能力), §7 (persistence) |
| `docs/03-current-architecture.md` | §4 (extraction), §7 (analysis/trace) |
| `crates/atlas-engine/crates/db/src/schema.rs` | 当前 schema v2 + migration 机制 |
| `crates/atlas-engine/crates/types/src/capability.rs` | 当前 capability model |
| `crates/atlas-engine/crates/types/src/summary.rs` | 当前 FunctionSummary 类型定义 |
| `crates/atlas-engine/crates/analysis/src/summary.rs` | 当前 SummaryBuilder 实现 |
| `crates/atlas-engine/crates/analysis/src/trace/virtual_edges.rs` | 当前 SummaryEdgeProvider |

### 实现顺序

1. Schema v3 (新表 + migration)
2. SummaryStore (持久化 CRUD)
3. SummaryBuilder 重构 (分离计算与存储)
4. CrossFunctionBridge (新模块)
5. SummaryEdgeProvider 委托给 CrossFunctionBridge
6. derive_capability_level 更新
7. CLI index/sync 集成
8. 测试

### Phase 1 实现边界约束

- **不要**修改 `dataflow_edges` 表的列定义或内涵
- **不要**修改 `Slicer` 或 `Locator` 的算法
- **不要**删除 `SummaryBuilder` 或 runtime fallback 路径（向后兼容旧 DB）
- **不要**在 Phase 1 实现级联失效（`invalidate_function` 仅删除本函数数据即可）
- **不要**修改任何 capability profile（Phase 2 的职责）
- **不要**提升 Cangjie 的 capability level

### 需要用户确认的问题

1. **是否接受 4 张摘要表（而非简化方案）？** 已确认：保留 `summary_call_arg_sources`。
2. **摘要计算时机：全量 index → 全局批处理，增量 sync/lazy → 逐文件增量。** 已确认。
3. **语言推送顺序：TS→JS→Java→Go→C#→Rust→PHP→Ruby→Kotlin→Python→C→C++→ArkTS。** 已确认。
4. **Cangjie 保持 Symbolic，不参与 DataflowFull。** 已确认。
