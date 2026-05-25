# Lazy Index Architecture & Constraints

> **临时文档** — 实现完成后应合并到正式架构文档中。
>
> 分支: `feat/lazy-index` | 创建: 2026-05-25 | 状态: 待实现

---

## 1. 问题定义

### 1.1 核心问题

对 Linux kernel（~60K C 文件）这类超大项目：

1. **全量 structural extraction 太慢**（~600-800s）
2. **全量 index 产生巨大数据库**（2-5GB）
3. **当前 lazy dataflow 只解决了"深度"维度**（dataflow 按需），未解决"范围"维度（只索引项目子集）

### 1.2 两个正交维度

```
             解析范围
  全项目 ←────────────────→ 局部

  ┌─────────────────────────────────┐
  │                                 │  解析深度
  │  Manifest     Structural  Full  │  (每文件提取多少层)
  │  (顶层符号)   (符号+引用) (+dataflow)
```

- **范围**: 索引哪些文件（全项目 / scope 子目录 / 按需单个文件）
- **深度**: 每个文件提取多少信息（manifest / structural / full）

现有 Atlas 已解决 structural vs full 的深度选择（`ExtractionMode`），但范围始终是全项目。

---

## 2. 三阶段演进路径

```
P0: A (scope index)           P1: C-lite (manifest)        P2: B (lazy structural)
┌─────────────────────┐      ┌──────────────────────┐     ┌──────────────────────┐
│ --scope/--include    │ ──→ │ ExtractionMode::      │     │ LazyStructuralService│
│ scope metadata       │      │   Manifest            │     │ CandidateProvider    │
│ completeness hint    │      │ file_index_layers 表  │ ──→ │ ensure_structural    │
│                      │      │ 顶层符号进 symbols 表  │     │ scoped resolve       │
│ 降低范围成本          │      │ 为 B 提供候选源       │     │ partial graph        │
└─────────────────────┘      └──────────────────────┘     └──────────────────────┘
```

**每阶段可独立交付和验证**，后续阶段基于前阶段构建。

---

## 3. P0: Scope Index（范围限制索引）

### 3.1 目标

允许用户只索引项目子集，立即降低大型项目的 index 时间和 DB 体积。

### 3.2 接口契约

#### CLI 接口

```bash
# 通用 include（支持多值 + glob）
atlas index \
  --include "drivers/net/**" \
  --include "net/**" \
  --include "include/net/**" \
  --exclude "drivers/net/wireless/**"

# scope 语法糖（目录 → glob）
atlas index \
  --scope drivers/net \
  --scope net \
  --include "include/net/**"

# scope 转换规则: "drivers/net" → "drivers/net/**"
```

#### 参数变更

| 参数 | 当前 | 新 |
|------|------|-----|
| `--include` | `Option<String>` | `Vec<String>` |
| `--scope` | 不存在 | `Vec<String>` (新增) |
| `--exclude` | `Vec<String>` | 不变 |

#### Scope → Glob 转换

```
drivers/net     → drivers/net/**       (bare dir → recursive glob)
kernel/sched    → kernel/sched/**      (bare dir → recursive glob)
drivers/net/*   → drivers/net/*        (已有 glob，不变)
```

### 3.3 数据层

#### Metadata 记录

```sql
-- 记录当前索引的 scope 信息
INSERT OR REPLACE INTO project_metadata (key, value)
VALUES ('indexed_scope', '["drivers/net/**", "net/**", "include/net/**"]');
```

每次 `atlas index` 运行时更新。`atlas status` 显示。

#### Completeness 标记

所有查询结果必须附加：

```json
{
  "completeness": "partial_scope",
  "indexed_scope": ["drivers/net/**", "net/**"],
  "note": "Results are limited to indexed scope. Files outside scope may contain additional references."
}
```

CLI 输出：在 `atlas status` 和 search/context 结果显示 scope 提示。

### 3.4 实现位置

| 文件 | 改动 |
|------|------|
| `crates/atlas-cli/src/commands/index.rs` | `fn run`: 参数 `includes: &[String]` 替代 `include: Option<&str>` |
| `crates/atlas-cli/src/main.rs` | clap 参数: `--include Vec<String>`, `--scope Vec<String>` |
| `crates/atlas-mcp/src/tools/index.rs` | `handle_index`: include 参数从单值改多值 |
| `crates/atlas-engine/crates/discovery/src/...` | `DiscoveryConfig.include_patterns` 已支持 `Vec<String>`，无需改动 |
| `crates/atlas-cli/src/commands/status.rs` | 显示 `indexed_scope` metadata |
| Schema 变更 | 无 |

### 3.5 验证标准

```bash
# 1. scope index 功能
atlas init ~/linux
atlas index --scope drivers/net --scope net

# 验证
atlas status | grep "Index scope"     # 应显示 scope 信息
atlas search tcp_sendmsg --json | jq '.completeness'  # 应返回 partial_scope

# 2. 只索引 scope 内文件
# 检查 DB: 文件数应 << 总文件数
atlas status | grep "Files:"

# 3. 多次 include 正确计算
atlas index --include "a/**" --include "b/**"
# 应包含 a/ 和 b/ 的所有文件

# 4. scope 元数据在 open_project 时可用
# MCP: 连接项目后, status response 包含 indexed_scope
```

---

## 4. P1: Manifest Extraction（轻量全局索引）

### 4.1 目标

以最小成本建立全局候选文件来源，为 P2 的 LazyStructuralService 提供查询入口。

### 4.2 关键架构决策

| 决策 | 选择 | 明确拒绝的替代方案 | 原因 |
|------|------|-------------------|------|
| 符号提取方式 | tree-sitter parse + 顶层 query | regex scanner + `symbol_stub` 表 | 避免两套符号体系；tree-sitter parse 成本可接受（单 C 文件 < 1ms）；macros 等边界 regex 无法正确处理 |
| CLI 入口 | `atlas index --analysis manifest` | 新命令 `atlas scan` | 架构一致性：都是索引行为，仅深度不同 |
| 符号存储 | 顶层符号写入现有 `symbols` 表 | 新建 `symbol_stub` 表 | 统一符号来源，避免查询时走两套逻辑 |
| Candidate fallback | symbols 表 + ripgrep（两层） | symbols + stub + ripgrep（三层） | ripgrep 在 60K 文件上找符号 < 1s，不需要中间近似层 |

### 4.3 ExtractionMode::Manifest

```rust
pub enum ExtractionMode {
    /// 新增 — 仅提取顶层符号
    ///
    /// 提取阶段:
    ///   parse ✓, symbols (top-level only) ✓,
    ///   references ✗, scopes ✗, imports ✗,
    ///   dataflow ✗, cfg ✗, callsites ✗, exports ✗
    Manifest,

    /// 现有 — 默认索引 (符号+引用+作用域等)
    Structural,

    /// 现有 — 按需 dataflow
    LazyDataflow { window: LazyWindow },

    /// 现有 — 全量
    Full,
}
```

#### 顶层符号定义

对每种语言，只提取 declaration/definition 在文件顶级作用域（不进入函数体/类体内部）的：

| 语言 | 提取的符号类型 |
|------|---------------|
| C/C++ | function_declarator, struct_specifier, union_specifier, enum_specifier, type_definition, preproc_def（宏）|
| Rust | function_item, struct_item, enum_item, trait_item, impl_item, mod_item, macro_definition |
| TypeScript | function_declaration, class_declaration, interface_declaration, type_alias_declaration, enum_declaration |
| Python | function_definition, class_definition |
| Go | function_declaration, type_declaration (struct/interface) |
| Java | class_declaration, interface_declaration, enum_declaration, method_declaration (top-level class members) |

**不提取**: 局部变量、函数体内调用、if/for 内定义、匿名函数/lambda（除非在顶层）。

**引用**: Manifest 模式下不提取任何 references。牺牲跨文件引用解析以换取速度。

### 4.4 数据模型

#### 新增表: `file_index_layers`

```sql
CREATE TABLE file_index_layers (
    file_id       BLOB NOT NULL,
    layer         TEXT NOT NULL,  -- 'manifest' | 'structural' | 'dataflow'
    content_hash  TEXT NOT NULL,
    status        TEXT NOT NULL,  -- 'complete' | 'partial' | 'failed'
    updated_at    TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (file_id, layer),
    FOREIGN KEY (file_id) REFERENCES files(file_id) ON DELETE CASCADE
);
```

**设计约束**:
- 不支持 `indexing` 状态持久化。进程运行中的状态仅在内存中；进程崩溃后，hash check 发现缺失 layer 记录即归入 dirty。
- `analysis_artifacts` 表保持不变，继续用于 dataflow unit 级别的缓存。
- `files.status` 保持不变（现有语义），由 layer 表补充。

#### Manifest 模式下的符号标记

Manifest 模式下提取的符号写入 `symbols` 表，但通过 `symbols.layer` 字段区分：

```sql
ALTER TABLE symbols ADD COLUMN layer TEXT NOT NULL DEFAULT 'structural';
-- manifest 模式的符号: layer = 'manifest'
-- structural 模式的符号: layer = 'structural'
```

这允许查询时区分符号来源：
- `layer = 'structural'` → 精确符号，可直接用于 resolve
- `layer = 'manifest'` → 候选符号，指示可触发 lazy parse 的文件

### 4.5 增量 sync 行为

`atlas sync` 检测到文件变更时：
1. 删除该文件在 `file_index_layers` 中的所有 layer 记录
2. 按当前 mode 重建：manifest 模式重建 manifest 层；structural 模式重建 manifest + structural 层

### 4.6 性能目标

| 指标 | 目标 | 测量方法 |
|------|------|---------|
| Manifest index 时间 | < 120s (60K C 文件) | PhaseTimer |
| Manifest DB 体积 | < 150MB (60K C 文件) | `get_stats()` 或文件系统 |
| 单文件 manifest parse | < 2ms (avg) | PhaseTimer per-file |
| Manifest vs Structural 时间比 | < 1:5 | 同一项目的 PhaseTimer 对比 |

### 4.7 验证标准

```bash
# 1. manifest index 功能
atlas index --analysis manifest

# 验证: DB 里只有顶层符号
atlas status  # 检查: 符号数应远少于 structural 模式
atlas search tcp_sendmsg  # 应找到函数声明（如果有）

# 2. 不提取内部符号
# 项目中有 tcp_sendmsg 函数体内调用 ip_queue_xmit 时
atlas search ip_queue_xmit
# 如果 ip_queue_xmit 只在函数体内被调用，manifest 中不应有它的符号记录

# 3. file_index_layers 正确记录
# DB 查询: 每个 indexed 文件应有 layer='manifest', status='complete'

# 4. sync 行为正确
# 修改一个文件后 atlas sync --analysis manifest
# 检查: layer 更新，hash 匹配
```

---

## 5. P2: Lazy Structural（按需深度索引）

> **注意**: P2 是 P1 之后的阶段。以下设计为前瞻性约束，P0 和 P1 的实现应为此留出接口空间。

### 5.1 目标

查询时按需触发完整的 structural extraction（symbols + references + scopes + callsites），替代全量预先索引。

### 5.2 架构组件

```
Query (search/context/trace)
    │
    ▼
LazyStructuralService
    │
    ├── CandidateProvider
    │   ├── symbols 表（manifest 层 + 已有 structural 层）
    │   └── ripgrep fallback（无匹配时）
    │
    └── StructuralLoader
        ├── ensure_structural_for_file(file_id)
        ├── ensure_structural_for_symbol(name, budget)
        ├── ensure_structural_for_scope(scope, budget)
        └── 写入: file_index_layers(layer='structural', status='complete')
```

### 5.3 接口约束（P0/P1 实现时的预留）

#### LazyStructuralService trait（P2 实现前为 placeholder）

```rust
pub trait CandidateProvider {
    fn candidates_for_symbol(&self, name: &str, budget: CandidateBudget) -> Result<Vec<FileId>>;
    fn candidates_for_path(&self, path: &str) -> Result<Vec<FileId>>;
}

pub struct CandidateBudget {
    pub max_candidates: usize,  // 默认: 20
    pub max_search_ms: u64,     // 默认: 1000
}
```

**P0/P1 不需要实现这个 trait**，但需要确保 `symbols` 表可以作为候选源。具体来说：
- P1 manifest 模式写入 `symbols` 时标记 `layer = 'manifest'`
- P2 实现时，CandidateProvider 通过 `SELECT file_id FROM symbols WHERE name = ? AND layer IN ('manifest', 'structural')` 获取候选文件

#### 增量 resolve（预留接口）

P2 不能调用 `resolve_all()`，需要增量版本：

```rust
// 预留接口（P0/P1 不需要实现）
pub fn resolve_for_files(store: &Store, file_ids: &[FileId]) -> Result<ResolutionStats>;
pub fn build_edges_for_files(store: &Store, file_ids: &[FileId]) -> Result<GraphBuilderStats>;
```

### 5.4 Completeness 语义

所有查询结果必须返回 completeness 信息：

```rust
pub enum Completeness {
    Complete,                                    // 全项目 structural 完成
    CompleteWithinScope(Vec<String>),             // scope 内完成
    PartialBudgetExceeded { indexed: usize, candidate: usize },  // budget 截断
    PartialMissingCandidates { missing: usize },  // 候选未完全覆盖
}
```

---

## 6. 架构决策汇总

### 6.1 达成共识的决策

| # | 决策 | 理由 |
|---|------|------|
| D1 | P0 → P1 → P2 顺序实现 | 每阶段独立可验证，后阶段依赖前阶段 |
| D2 | C-lite 必须在 B 之前（不是 B 单独做） | B 的 CandidateProvider 需要全局符号候选源 |
| D3 | 用 `atlas index --analysis manifest` 而非新命令 | 语义一致性 |
| D4 | B 必须包含 partial completeness semantics | 避免误导用户 |
| D5 | B 的 resolve 和 graph build 必须增量（非 `_all()`） | 避免每 lazy 一个文件都全量重建 |
| D6 | 跨文件引用解析在初版不做完整保证 | 部分索引状态下全局解析不完整 |

### 6.2 明确被否定的替代方案

| # | 被否决的方案 | 否决原因 |
|---|-------------|---------|
| R1 | C-lite 用 regex scanner 而非 tree-sitter | 引入两套符号体系；regex 在 C/C++ macros 场景不可靠 |
| R2 | 新建 `symbol_stub` 表 | 导致查询逻辑分叉；manifest 模式符号应直接写入 `symbols` |
| R3 | CandidateProvider 三层 fallback（stub 作为中间层） | ripgrep 在 60K 文件上 < 1s，不需要中间近似层 |
| R4 | `file_index_layers` 包含 error/node_count/budget_exceeded/`indexing` 状态 | 过度设计；error 记录到 tracing；node_count 动态查询；budget_exceeded 留在 analysis_artifacts；进程崩溃后 hash mismatch 自动处理 |
| R5 | P1/P2 合并到一个阶段 | 失去可验证中间态；增加实现风险 |

### 6.3 设计的约束（不要违反）

| 约束 | 说明 |
|------|------|
| 符号只有一种来源 | `symbols` 表是唯一符号数据源。manifest/structural 模式的区别仅在于 `layer` 列 |
| 不新增 CLI 顶级命令 | 用 `--analysis` 参数控制提取深度，用 `--scope`/`--include` 控制范围 |
| layer 表不与 analysis_artifacts 重复 | `file_index_layers` 是 file 级别，`analysis_artifacts` 是 unit 级别 |
| 每个阶段独立可交付 | P0 完成即可发布，P1 叠加后可发布，P2 叠加后可发布 |

---

## 7. 实施顺序

### P0（当前实现）

```
1. CLI: --include Vec<String>, --scope Vec<String>
2. CLI: scope → glob 转换
3. CLI: completeness 提示（scope metadata 读写）
4. MCP: include 参数多值化
5. status 命令: 显示 indexed_scope
6. 测试: scope index 端到端
```

### P1（后续实现）

```
1. ExtractionMode::Manifest 枚举 + phase matrix
2. tree-sitter manifest query（每语言顶层符号 query 文件）
3. symbols.layer 列 + migration
4. file_index_layers 表 + migration
5. extract_file_with_mode 支持 Manifest
6. CLI: atlas index --analysis manifest
7. sync: manifest 层增量更新
8. 测试: manifest index 端到端
```

### P2（长期实现）

```
1. LazyStructuralService (placeholder → 完整实现)
2. CandidateProvider: symbols 查询 + ripgrep fallback
3. StructuralLoader: ensure_structural_for_file / for_symbol / for_scope
4. 增量 resolve_for_files / build_edges_for_files
5. Completeness 类型 + 所有查询响应集成
6. MCP lazy structural 自动触发
7. 测试: lazy structural 端到端
```

---

## 8. 移交 Coder

### 8.1 P0 实现前必须读取的文件

| 文件 | 读它的原因 |
|------|-----------|
| `docs/06-lazy-index-architecture.md` | 本文件 — 架构约束 |
| `crates/atlas-cli/src/commands/index.rs` | 了解 `fn run` 当前签名和流程 |
| `crates/atlas-cli/src/main.rs` | 了解 clap 参数定义方式 |
| `crates/atlas-engine/crates/discovery/...` | 了解 `DiscoveryConfig` 已有的 `include_patterns` |
| `crates/atlas-mcp/src/tools/index.rs` | 了解 MCP index 参数定义 |

### 8.2 P0 实现的边界约束

- **不要**创建新表或修改 schema
- **不要**创建新的 CLI 命令（只用 `--include`/`--scope` 参数）
- **不要**实现 P1 或 P2 的逻辑
- `--scope` 只是语法糖，内部统一转成 `include_patterns`
- scope metadata 用现有的 `set_metadata("indexed_scope", ...)` 机制
- completeness 提示可以先做 CLI 输出（`eprintln!` 或结果注释），不做结构化 JSON（留给 P2）

### 8.3 需要确认的问题

1. `--scope` 和 `--include` 的关系：scope 是否独立于 include（即用户能否同时用 `--scope drivers/net --include "include/**"`）？**架构决策**: scope 只是 include 的语法糖，两者合并成 `include_patterns`。
2. 已有的 `--include` 单值参数：是否保留向后兼容？**建议**: 改为多值，旧用法 `--include "pattern"` 继续有效（clap `Vec<String>` 兼容单值）。
