# Atlas 架构文档

本文是 Atlas 的**单一权威架构文档**：只写**当前**设计原则、不变量与实现事实。  
版本演进、迁移与破坏性变更见 [`CHANGELOG.md`](../CHANGELOG.md)，不在本文复述。

> 当前基线：Atlas `1.5.2`、SQLite Schema V2、16 个 Cargo package、14 种默认语言、15 个 MCP 工具。版本号以 workspace manifests 为准，schema 以 `db::CURRENT_SCHEMA_VERSION` 为准，语言能力以 `LanguageCapabilityProfile` / `atlas doctor` 为准，MCP 工具面以 `make_all_tools()` 为准。

## 1. 总体原则

1. Atlas 是 CodeGraph-inspired，不是 CodeGraph-compatible。
2. Rust 实现使用 trait、newtype ID、enum、immutable facts、batch write、read snapshot 和 Rayon。
3. SQLite 是持久化源（`.atlas/atlas.db`）；内存图只作为查询加速和分析工作集。
4. MCP 是一等入口；CLI、MCP、context 输出都必须可限制大小。
5. 所有启发式语义结果必须可解释，不能把低置信度结果伪装成精确结果。
6. 分析等级相关改动必须验证完整入口矩阵：CLI / shared filesync / sync（Index）、Focus materialize ensure、高层 Engine、raw analysis consumer。任何模式或 capability/status 变化都不能只验证单一路径。
7. **终态必然可达**：每次工具调用必须收敛到终态；不存在永久 `building` / `wait` 状态。MCP 响应的 `analysis.retry_after_ms` 必须最终变为 null。
8. **信号最小**：响应中每个字段，Agent 必须有明确的 consume 路径；非 trace 公共信封不暴露 `partial_result`、`background_refinement`、`analysis.state` 等伪信号。冻结的 trace 内层契约继续保留自己的 `partial_result`。
9. **内部状态不透出**：引擎层专有概念（`AnswerQuality`、closure ID、调度器优先级）不进入 MCP 公共响应。
10. **事实，非指令**：响应字段提供事实（缺了什么），不提供 Agent 无法执行的指令（如"去索引这个"）。
11. **三模式共享同一结构**：TUI / MCP+progress / MCP-no-progress 使用同一响应信封；progress token 只增加观测通知，不改变终态、重试或恢复语义。
12. **精度术语分层（强制）**：见 §1.1。禁止无限定的 `mode` / `full` / `index_mode` 单独出现在 API、日志与文档标题；禁止两个不同语义的类型共用 `IndexMode` 一名。

### 1.1 精度与能力术语分层

概念分五层，**禁止跨层复用同一词根**：

| 层 | 权威类型（代码） | 含义 | 禁止混为 |
|----|------------------|------|----------|
| L0 语言理论 | `LanguageCapabilityProfile` + `FeatureMatrix`；派生摘要 `CapabilityLevel` | 语言**能**分析到哪 | 库里已有 facts |
| L1 已物化证据 | **`FactCoverage`** bits；`read_catalog_tier()` 派生 **CatalogTier** 字符串 | 库/文件**有**哪些 facts | 语言 capability |
| L2 抽取处方 | `ExtractionMode`（口语 **ExtractRecipe**） | 本次抽取执行哪些 phase | 读路径控制面 |
| L3 运行时控制面 | **`AccessStrategy`** `{ FullCache, Focus }`；**`PipelineGrade`** `{ Manifest, Structural, Full }`；**`EdgeProvenance`** `{ RepoCanonical, FocusScoped }` | 怎么查 / 配置哈希 / 边出处 | 抽取 phase、答案质量 |
| L4 答案质量（内部） | **`AnswerQuality`**+ `CoverageTier` + `SemanticConfidence` | 本次查询覆盖×置信 | MCP 公共字段 |

**L3 控制面类型（必须使用下列名称，禁止再引入 `IndexMode`）**：

| 类型 | 变体 |
|------|------|
| `AccessStrategy` | `FullCache` \| `Focus` |
| `PipelineGrade` | `Manifest` \| `Structural` \| `Full` |
| `EdgeProvenance` | `RepoCanonical` \| `FocusScoped` |

**`Full` 三义消歧**（必须带限定）：

- `ExtractionMode::Full` — 本次抽取含 dataflow/CFG
- `AccessStrategy::FullCache` — 存在 finalize 的全库缓存可读路径
- CatalogTier 字符串 `"full"` — `read_catalog_tier()` 聚合后的库状态标签
- `CapabilityLevel::DataflowInterproc` — 语言派生摘要（**不含** cfg 要求；真值看 `FeatureMatrix`）

**MCP 公共词汇（冻结）**：`capability`（语言+feature）、`analysis.*`、`gaps`、`coverage_counts`、`note`、`query_id`、trace 内层 `partial_result`。  
**禁止**进入公共 JSON：`AnswerQuality`、`AccessStrategy` 原始枚举名、`PipelineGrade`、`EdgeProvenance`、`FactCoverage` 原始字段。

**状态字段**：`project(status)` 使用 JSON 键 **`catalog_tier`**（`read_catalog_tier()` 派生字符串，L1）；不是 `AccessStrategy`。

## 2. 模块边界与依赖方向

### 2.1 Crate 结构

项目是 16 个 Cargo package 的 workspace：

```text
crates/
  atlas-engine/        facade crate，re-export types/db/extraction/resolution/graph/analysis/search/context/filesync/focus_materialize, dossier
    crates/types/      ID、enum、IR、binding、dataflow、CFG、trace 查询类型、capability profiles
    crates/workspace/  ProjectRoot、WorkspacePaths、SourcePath
    crates/db/         SQLite schema v2、Store API、readers、schema 初始化基础设施
    crates/extraction/ tree-sitter 解析、query、scope、semantic binder、lexical binder、dataflow、CFG、worker pool
    crates/resolution/ builtin filter、scope/container/import/include/name matching、PathAliasResolver
    crates/graph/      GraphBuilder、GraphSnapshot、GraphEngine
    crates/analysis/   变量来源追踪与调用路径查询、SummaryBuilder、CrossFunctionBridge
    crates/domain_rules/ 语言无关 domain rule store/match/learning 核心
    crates/search/     FTS5、LIKE/fuzzy、query parser、scoring
    crates/context/    Agent context builder (Markdown)
    crates/filesync/   file discovery、change detection、file lock、watcher（**Index 路径**）
    crates/focus_materialize/  Focus **内部** on-demand dataflow materialize（包名与 Focus 叙事对齐）
    crates/dossier/    Symbol Dossier builder
  atlas-mcp/           MCP server (rmcp stdio JSON-RPC)、15 open-first **Focus** tools
  atlas-cli/           CLI binary + commands + integration tests（含 `atlas index`）
```

### 2.1.1 Index 与 Focus（对外只两种查询时策略叙事）

| 路径 | 产品语义 | 实现要点 |
|------|----------|----------|
| **Index** | **简单、通用**的预物化：scope/全仓按 `ExtractionMode` 写入 SQLite 并 finalize | `filesync::IndexPipeline` / `atlas index`；**不**依赖 Focus 控制面 |
| **Focus** | **查询时唯一复杂路径**：意图驱动热点与闭包，**局部优先**物化，使闭包内体验≈该邻域已被 Index | `FocusRuntime` + 内部 materialize；MCP open-first 默认 |

**AccessStrategy（L3）：** `FullCache`（Index 已 finalize 且 catalog 够富）| `Focus`（否则查询时局部加强）。

**产品路径 vs 机制类型**

| 名称 | 层 | 含义 |
|------|----|------|
| `Index` / `Focus` | 产品 | 预物化 / 查询时局部加强 |
| `FocusMaterialize` | Focus 内部栈 | 单配置 structural + dataflow ensure + rebuilder |
| `LazyDataflowService` / `LazyStructuralService` | 机制 | 按需 ensure 写库（CS lazy；**不是** AccessStrategy） |
| `LazyWindow` / `LazyBudget` | 机制 IR | 按需窗口与预算 |
| `ExtractionMode::LazyDataflow` | L2 处方 | 增量 unit dataflow/CFG |
| 包 `focus_materialize` | 包 | Focus 内部 on-demand dataflow |

**禁止**把 “Lazy” 当作第三条产品路径。  
构造：生产路径只经 `FocusMaterialize::open`；`with_structural_rebuilder` 为 `#[doc(hidden)]` 工厂。MCP 多 runtime 必须 `Engine::from_materialize` / `AnalysisRuntime::from_materialize` 共享同一栈。

### 2.2 依赖方向（严格无环）

```text
atlas-cli → atlas-engine, atlas-mcp
atlas-mcp → atlas-engine
atlas-engine → types, workspace, db, extraction, resolution, graph, analysis, search, context, filesync, focus_materialize, dossier, domain-rules
filesync → graph, resolution, extraction, analysis, db, types, workspace
  （filesync 不得依赖 focus 控制面 / FocusRuntime / scheduler）
search / context → graph, db, types
analysis → db, types, workspace, domain-rules
domain-rules → db
graph → db, types
resolution → db, types, workspace
extraction → types
db → types
workspace → (stdlib + anyhow)
types → (anyhow, blake3, hex, rusqlite, serde)
```

### 2.3 模块职责边界

| 模块 | 负责 | 不负责 |
|------|------|--------|
| `types` | ID 类型、enums、IR 结构、capability profiles | 不依赖上层模块 |
| `workspace` | ProjectRoot、SourcePath、路径抽象 | 不承载语言语义规则 |
| `db` | schema、初始化、读写 | 不承载语言语义规则 |
| `extraction` | 单文件 tree-sitter facts 抽取 | 不做跨文件 resolution |
| `resolution` | 更新 resolved facts | 不直接承担展示格式 |
| `graph` | 从 resolved facts 构建 symbol graph | 不混入 dataflow/CFG |
| `analysis` | 消费 dataflow、CFG 和 call graph；trace/slicing | 不破坏底层 facts |
| `domain_rules` | 语言无关 rule 存储、匹配、学习候选、registry 校验 | 不解释 C/C++ ownership、Rust safety 等语言语义 |
| Focus materialize（`focus_materialize` crate + `focus/materialize`） | Focus 方案内按需 structural/dataflow 物化、budget | **不是**对外产品；不改变 extraction 语义 |
| Focus 控制面 | `FocusRuntime`、闭包、热点、调度、bootstrap | 不实现 tree-sitter；不替代 IndexPipeline |
| `dossier` | 聚合符号源码、调用证据、关系与文件上下文 | 不触发项目级索引策略 |
| `filesync` | discovery、dirty detection、共享索引/增量管线、清理与锁 | 不承载 Focus 控制面或 CLI/MCP 展示逻辑 |
| `cli` / `mcp` | 只编排能力 | 不内嵌解析、resolution 或分析算法 |

## 3. ID 约束

所有持久化 ID 必须 deterministic，禁止 UUID/自增作为核心身份。

```text
FileId       = blake3(project_relative_path)
SymbolId     = blake3(file_id + language + symbol_path + kind + stable discriminator)
ScopeId      = blake3(file_id + parent/scope path + range/kind)
ReferenceId  = blake3(file_id + kind + source/range + reference_text)
EdgeId       = blake3(source + target + kind + ref_id/provenance)
BindingId    = blake3(file_id + scope_id + kind + name + start_byte)
DataNodeId   = blake3(file_id + function_id? + kind + name? + access_path? + start_byte)
CfgNodeId    = blake3(function_id + kind + start_byte)
```

约束：
- `ReferenceId` 必须包含 `ReferenceKind`，避免同 range 的 call/field captures 冲突。
- 不得用 line number 作为稳定 ID 核心。
- ID 类型必须分层，不能用 `SymbolId::default()` 伪装 dataflow node。

### 3.1 SymbolId 内外分界

`SymbolId` 是引擎内部确定性标识符（Blake3 hash），**不暴露给 MCP 外部契约**。外部所有
需要符号引用的接口（MCP 工具参数、查询结果、候选消歧列表）统一使用 `SymbolSelector`：

```json
{
  "qualified_name": "atlas_engine::Engine",
  "file_path": "src/lib.rs",
  "line": 42,
  "kind": "function",
  "language": "rust"
}
```

`SymbolSelector` 的字段按计分优先级排序（qualified_name > file_path > line > kind > language），
错误字段不会阻塞正确匹配——只影响候选排序。详见第 10.6 节。

## 4. 抽取约束

```text
tree-sitter 0.26 parser
  → per-language .scm queries
  → LanguageAdapter normalization
  → FileFacts
```

约束：
- 不实现大型 `GenericExtractor`。
- LanguageAdapter 不填跨文件语义结果。
- Adapter 不手写重复的 enclosing function/source_symbol 逻辑；source、scope、binding 由 binder 统一处理。
- 单文件失败必须结构化记录，不中断项目索引。
- ArkTS 核心语言是 TypeScript 的静态类型约束子集，前端复用 TypeScript grammar，但
  language 必须是 `arkts`。parser slot 以等长
  `struct` → `class ` 归一化保留声明式组件的字段、方法和 scope；ArkUI trailing-block
  是 UI 声明式扩展，仍可能产生局部 parse error，ArkTS normalizer 必须消除伪 method 并
  恢复 call ownership。语言边界以华为官方 [TypeScript 到 ArkTS 迁移规则](https://developer.huawei.com/consumer/cn/doc/harmonyos-guides/typescript-to-arkts-migration-guide)
  为依据，不把 ArkUI 语法错误归因于 ArkTS 核心语法。
- C/C++ 是 best-effort，不承诺完整 preprocessing、模板、重载。
- 所有 14 种语言均默认编译。

## 5. Fact 模型约束

`FileFacts` 包含：

```text
file metadata, symbols, scopes, references, imports/exports,
callsites, bindings/binding_uses, data_nodes/dataflow_edges,
cfg_nodes/cfg_edges, structural facts, diagnostics
```

不变式：
- 同一 `FileFacts` 中的 facts 必须属于同一个 file。
- range 必须包含 byte offset 和 line/column。
- references 永不因为 resolved 而删除；unresolved references 必须保留。
- callsite 必须能回溯到 reference location。
- dataflow 使用 `DataNodeId → DataNodeId`，6 字段完整 TextRange。
- CFG 节点必须属于同一 function，函数 CFG 应有 Entry/Exit。

### 5.1 Symbol Signature Contract

`SymbolDef.signature` 是跨语言符号事实，由 language adapter 在 extraction 阶段产生并持久化到
`symbols.signature`。下游 graph、context、CLI 和 MCP 只能透传该 DB fact，不得在展示层重新推断签名。

签名格式：
- 单行字符串，使用 compact whitespace normalization。
- 不包含符号名，只包含接口形状信息，例如参数列表、泛型参数、返回类型或语言等价形式。
- 适用于 `function`、`method`、`constructor` 等可调用符号；类型、变量、字段等天然无调用签名的符号可为 `null`。

`signature: null` 只允许两类情况：
- 符号类型天然无签名。
- 该语言/语法构造当前明确 unsupported，并通过 golden 或集成测试体现。

新增或修改语言 adapter 时必须通过 extraction golden 覆盖至少一个 function/method signature，确保 full index 后 MCP
`symbol(view="detail")` 能从 DB 返回签名，而不是在 MCP 层补丁式生成。

## 6. Persistence 约束

### 6.0 存储分层模型（Storage Hierarchy）

Atlas 的存储模型是一个**单持久 SQLite 数据库** `project/.atlas/atlas.db`，内部通过 SQLite 引擎自身的页面缓存机制实现透明的分层读取。对外只有一个打开项目的语义：

```text
open_project(project_path)
  → Store::open_db(project/.atlas/atlas.db)

MCP Tool
  → ActiveProject
    → Store (single persistent SQLite DB)
      → level 1: SQLite in-process page cache (64 MB, transparent)
      → level 2: .atlas/atlas.db WAL file (256 MB mmap, durable)
      → level 3: focus extraction (on-demand from source files)
```

**分层语义**：

| 层级 | 名称 | 介质 | 特性 |
|------|------|------|------|
| L1 | Page Cache | SQLite进程内存 | 64 MB 透明页面缓存，LRU 由 SQLite 自动管理 |
| L2 | Durable DB | `.atlas/atlas.db` | WAL 日志、256 MB mmap、跨会话持久 |
| L3 | Focus Extraction | 源码文件系统 | 按需 structural/dataflow 提取，结果写回 L2 |

**核心约束**：

1. `open_project` 不再暴露 `storage` 参数。始终使用单持久 SQLite DB；内部存储细节对 MCP 客户端不可见。
2. 读路径：先查 SQLite 页面缓存（L1），miss 后从 mmap/文件系统加载（L2），未索引符号通过 focus extraction 写入后查询（L3）。
3. SQLite 页面缓存由 `PRAGMA cache_size`（64 MB）和 `PRAGMA mmap_size`（256 MB）控制，应用层不复制缓存层。
4. 诊断信息通过 `project(status)` 的 `diagnostics.sqlite_cache` 字段暴露，包含 page_count、freelist_count、cache_size_kib、db_file_size_bytes 及其派生指标，但不成为正常 API 语义的一部分。
5. `Store::open_in_memory()` 保留用于测试，不用于生产查询路径。

**设计决策**：为什么不用应用层双 Store（memory + persistent）？

- SQLite 自带的 64 MB page cache 已经是一个高效的透明 L1 缓存，应用层再建缓存层是重复造轮子。
- `GraphSnapshot` 要求从单个 store 全量加载 symbols + edges 构建内存图，双 store 无法构建一致性视图。
- blake3 内容寻址 ID 在双 store 场景下虽然值相同，但 `file_id` 指向的 path/符号表可能不同步，导致上下文断裂。

**诊断暴露**：

```json
{
  "diagnostics": {
    "storage_hierarchy": {
      "model": "single persistent SQLite DB with transparent page cache",
      "layers": {
        "l1_page_cache": "SQLite in-process page cache — transparent, 64 MB default, LRU eviction",
        "l2_durable_db": "project/.atlas/atlas.db — WAL, 256 MB mmap, durable",
        "l3_focus_extraction": "on-demand structural extraction from source files"
      }
    },
    "sqlite_cache": {
      "page_count": 1234,
      "page_size_bytes": 4096,
      "freelist_count": 50,
      "cache_size_kib": 65536,
      "db_file_size_bytes": 5054464,
      "derived": {
        "total_db_kib": 4936,
        "used_db_kib": 4736,
        "file_on_disk_kib": 4936,
        "fragmentation_ratio": 0.041,
        "cache_coverage_ratio": 13.3
      }
    }
  }
}
```

- `fragmentation_ratio` = `freelist_count / page_count`：表示空闲页面占比；高值意味着 `VACUUM` 可压缩文件。
- `cache_coverage_ratio` = `cache_size_kib / total_db_kib`：表示页面缓存是否能在内存中覆盖整个 DB。>1.0 时所有页面理论可常驻内存。

### 6.1 Schema（当前版本：V2）

当前 schema 版本为 V2。软件处于快速原型期，新库以主 DDL 为准，不保留
旧 schema 运行时补丁路径。schema contract 改变时直接更新主
DDL、调用方和文档，并要求重新建库/重索引；不得在 `Store::init_schema`
中累积旧版本补丁路径。

主要表（28 张实体表 + 1 张 FTS5 索引，共 29 张）：

| 表 | 用途 |
|----|------|
| `files` | 文件元数据 |
| `symbols` | 符号定义（含 `layer` 字段：manifest/structural） |
| `scopes` | 作用域区域 |
| `"references"` | 引用使用（保留已解析字段） |
| `imports` | import/include 语句 |
| `symbol_edges` | 符号间语义边 |
| `callsites` | 调用表达式 |
| `bindings` / `binding_uses` | 词法绑定 |
| `data_nodes` / `dataflow_edges` | 数据流节点与边 |
| `cfg_nodes` / `cfg_edges` | 控制流图 |
| `function_summaries` | 函数摘要元数据 |
| `summary_param_reaches` | 参数 → 下游可达目标 |
| `summary_return_sources` | 返回值 → 上游来源 |
| `summary_call_arg_sources` | 调用参数 → 上游来源 |
| `domain_rules` | 语言无关的领域规则（匹配、学习、存储基础层；语义由 language consumer 解释） |
| `extraction_state` | 统一提取完成状态（文件级 layer + 单元级 dataflow/CFG） |
| `extraction_jobs` | 统一 lazy extraction job 去重与状态 |
| `project_metadata` | 项目级键值配置 |
| `symbols_fts` | FTS5 符号名索引 |
| `function_pointer_annotations` | C/C++ 函数指针 dispatch 注解 |
| `closure_generations` | Focus closure 代际追踪 |
| `closure_coverage` | Closure 覆盖度分布 |
| `reference_resolutions` | focus-scoped 引用解析结果 |
| `symbol_edge_candidates` | 候选图边（resolution 噪声缓冲） |
| `file_inventory` | 文件发现清单（增量检测用） |
| `symbol_hints` | 符号搜索提示（language/knowledge hints） |

约束：
- SQLite 使用 WAL。
- 写路径走事务和 batch write。
- 读路径可以短连接或 read API。
- symbol graph 与 dataflow graph 必须分表。
- `dataflow_edges` 保持纯 intra-procedural；跨函数事实仅存在于摘要表。

### 6.2 Domain Rules 通用化

`domain_rules` 不是 C/C++ ownership 子系统，而是语言无关的规则存储、匹配、学习候选和审计基础设施。所有语言语义都由 language registry 和 analysis consumer 解释。

核心原则：
- `domain_rules` crate 核心只处理 `DomainRule`、`PatternKind`、`RuleSource`、`RuleStatus`、`RuleMatch`、`LanguageRuleKinds`、`RuleLearningStrategy`。
- 核心 engine 不出现 ownership/free/alloc/cleanup/lifecycle 等 C/C++ 语义；这些语义只存在于 `analysis::ownership_rules::CppOwnershipRules` 等 consumer 中。
- 每种语言通过 `LanguageRuleKinds` 注册自己的 `rule_kind`、允许的 `pattern_kind`、builtin rules 和校验逻辑。
- `GenericRuleEngine` 只返回 `RuleMatch`；consumer 决定匹配结果意味着释放、分配、React hook、unsafe boundary 还是其他语义。
- learned rules 默认写入 `status='candidate'`，不参与匹配；用户 approve 后才变为 `enabled`。
- `language='*'` 只用于极少数通用规则，不做复杂跨语言继承。

`domain_rules` schema：

```sql
CREATE TABLE domain_rules (
  id            TEXT PRIMARY KEY NOT NULL,
  language      TEXT NOT NULL DEFAULT 'c',
  rule_kind     TEXT NOT NULL,
  pattern       TEXT NOT NULL,
  pattern_kind  TEXT NOT NULL DEFAULT 'exact',
  meta          TEXT,
  meta_version  INTEGER NOT NULL DEFAULT 1,
  source        TEXT NOT NULL,
  status        TEXT NOT NULL DEFAULT 'enabled',
  confidence    REAL NOT NULL DEFAULT 1.0,
  created_at    TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
```

状态机：

```text
candidate → enabled    用户批准 learned rule
candidate → rejected   用户拒绝 learned rule
enabled   → disabled   用户临时停用
enabled   → deprecated 规则过时但保留审计记录
```

匹配策略：
- `exact`: 精确字符串匹配。
- `prefix`: 前缀匹配。
- `suffix`: 后缀匹配。
- `glob`: glob 模式。
- `regex`: 高级正则模式，必须受缓存和上限约束。

当前 C/C++ 接入路径：

```text
domain_rules::GenericRuleEngine
  → CRegistry 注册 free_fn / alloc_fn / owned_pattern / cleanup_fn
  → analysis::CppOwnershipRules 解释 RuleMatch
  → lifecycle / lifecycle_proof / semantic impact 消费 ownership 视图
```

各语言接入和扩展规则见 [`domain-rules-language-guide.md`](./domain-rules-language-guide.md)。

## 7. 数据流

```text
Source files
  → discovery / file lock / worker
  → tree-sitter parse
  → query extraction through LanguageAdapter
  → scope tree
  → lexical binding (LexicalBinder)
  → local dataflow facts (DataFlowBuilder)
  → CFG facts (CfgBuilder)
  → SemanticBinder binds source_symbol, scope_id, binding
  → Store writes FileFacts
  → ReferenceResolver updates resolved_* fields
  → SummaryBuilder computes per-function summaries → summary tables
  → GraphBuilder writes symbol_edges
  → GraphSnapshot loads query graph
  → CLI / MCP / Search / Context / Analysis / Trace
```

### 7.1 Focus materialize（内部按需物化，非产品线）

在 **Focus** 查询时路径（及高层 `Engine::trace_*` 薄封装）中，analysis **按需** 加载 dataflow facts（而非全量预加载），通过机制类型 `LazyWindow` 控制分析范围。结构性按需提取 budget-capped (18s/30 files)；后台 preparse 使用更宽预算 (60s/100 files)。

**栈与构造（硬约束）**

- 唯一配置入口：`FocusMaterialize::open(store, project_root)`（structural + dataflow + 标准 structural rebuilder 一次焊死）。
- `FocusRuntime` / MCP `ActiveProject` / `Engine::from_materialize` / `AnalysisRuntime::from_materialize` 必须 **Clone 共享同一 Arc 栈**（`same_stack_as`）。
- `LazyDataflowService::with_structural_rebuilder` 仅跨 crate 工厂（`#[doc(hidden)]`）；禁止旁路标准 rebuilder。
- `Engine::from_store` 会 **新开** materialize 栈，仅适合 CLI/TUI 独占进程边界；MCP 热路径禁止用它并立第二套。

**写库语义**

- L2 处方 `ExtractionMode::LazyDataflow` = 增量 unit dataflow/CFG（机制名，不是 AccessStrategy）。
- unit 写路径 `replace_dataflow_for_unit`：无效 `function_id` 丢弃行；无效 `data_node.binding_id` **SET NULL 保留节点**（Focus 重抽 bindings 的 ScopeId 可能与 structural 库不一致，不得静默抽干 unit facts）。
- unit `capability_mask`（`FactCoverage`）是 truth source，禁止乐观 OR：
  - 成功 ensure 的 base：`MANIFEST | STRUCTURAL | DATAFLOW`
  - **`CALL_EDGES`**：仅当 **file-level structural（或 dataflow）层 complete 且 content_hash 与 `files` 一致**，且该 unit 有 structural callsite（caller=unit 函数）时置位（与 CFG 同构：存在性 + 新鲜度）
  - **`CFG`**：语言支持且 unit 实际产出 CFG nodes
  - 主路径与 prebuilt 缓存路径共用同一 helper（`unit_dataflow_capability_mask`）
- structural 与 Index 共用 `apply_post_extract_hooks`（Linux export/initcall 等）。

等级路径约束（与 §1.1 对齐）：
- L2 `ExtractionMode`：`Manifest` / `ResolutionSymbols` / `Structural` / `LazyDataflow` / `Full` — 抽取处方。
- L0 `CapabilityLevel`（`DataflowLocal` / `DataflowInterproc` 等）+ `FeatureMatrix` — 语言理论能力；**不是**库状态。
- L1 `FactCoverage` — 已物化证据 bits。
- L4 `AnswerQuality` — Focus 内部结果质量，不进 MCP 公共响应。
- L3 `AccessStrategy` — FullCache vs Focus 读路径；与 L2 `ExtractionMode::Full` 不同。
- 以上各层含义不同，**禁止混用**；禁止再引入第二个名为 `IndexMode` 的类型。
- 入口矩阵：`atlas index` / `filesync::IndexPipeline` / `atlas sync`（**Index**）；`FocusRuntime` + Focus materialize（**Focus**）；`Engine::trace_*`（物化薄调用）；raw `analysis::TraceEngine`（只读已有 facts）。
- 高层 `Engine::trace_variable` 经 Focus materialize 触发按需 dataflow；raw `analysis::TraceEngine` 只消费已存在 facts。
- `ExtractionMode::Full` 必须在 facts、summary、extraction_state、capability mask 和用户可见 CatalogTier 上都表现为完整分析。
- Focus 按需路径必须复用或严格对齐 structural facts（callsite、symbol、scope、content_hash、capability mask）；**闭包内 complete 文件/unit 的事实切片应 ≈ Index 同文件/同 unit**（验收见 `docs/testing.md` §2.6.2 N5）。

分析等级的长期语义如下，所有入口必须与此表保持一致：

| 等级 | 写入事实 | 继续阶段 | 用户可见要求 |
|------|----------|----------|--------------|
| `Manifest` | 仅顶层 manifest symbols | 不做 references、resolution、graph、summaries | 不能暴露或暗示 structural/dataflow 已完整 |
| `ResolutionSymbols` | symbols、imports、scopes、scope tree | 不做 references、dataflow、callsites | 仅作为 dependency/lazy resolution 目标层 |
| `Structural` | symbols、references、imports、scopes、callsites、exports、call edges | resolution + graph build | 能回答结构性搜索、context、caller/callee；不能宣称 dataflow/CFG |
| `LazyDataflow` | window 内 unit dataflow、binding uses、可用时 CFG | 不重写 structural facts | 必须记录 unit extraction state、budget/job 状态和 capability mask；入口再映射为 public retry/gaps |
| `Full` | Structural + 全文件 dataflow + 可用 CFG + summaries | resolution + graph + summary build | facts、summary、extraction_state、capability mask、status 必须全链路一致 |
| Raw analysis | 不触发 extraction | 只消费已有 DB facts | 调用者必须先准备所需 facts，不能隐式依赖 lazy |

Manifest 不是“低成本 definitions”。每个语言必须显式实现 top-level-only `manifest_query()`，或显式声明 Manifest 不支持该语言；禁止通过默认 `definition_query()` 把函数体内部符号写入 manifest 层。新增语言时，manifest、structural、lazy dataflow 三条路径必须一起登记。

提取层能力集中通过 `extraction_state.capability_mask` 表达，不把 precision 字段扩散到每个 symbol/reference/edge：

| Bit | Capability | 含义 |
|-----|------------|------|
| 0 | `manifest` | 顶层符号可用 |
| 1 | `structural` | 完整 symbols/scopes/references/callsites 可用 |
| 2 | `call_edges` | callsites 已解析并构建调用边 |
| 3 | `cfg` | 函数级 CFG 可用 |
| 4 | `dataflow` | intra-procedural dataflow 可用 |
| 5 | `summaries` | inter-procedural function summaries 可用 |

`field_lifecycle`、`branch_diff`、`ownership_proof` 属于 analysis 结果能力，不进入 extraction mask。

`summaries` bit 代表 summary tables 已针对相关函数构建完成；只有成功执行 summary build 后才能设置。不能仅因为 extraction mode 是 `Full` 就推断 summaries 可用。

### 7.2 跨函数桥接（DataflowInterproc）

Schema V2 包含持久化摘要层：

```
dataflow_edges    = intra-procedural, fine-grained, direct edges
function_summary  = intra-procedural, transitive-closure, per-function
trace             = inter-procedural, by composing summaries and/or runtime bridges
```

- `SummaryBuilder` 从 dataflow_edges BFS 计算函数摘要（**Full** 管线 / summary phase）。
- 摘要表支持全量与按函数增量构建；sync 时失效受影响行并重建。
- `trace_variable` 等能力门控 **local dataflow**（非 summaries）。跨函数边由 `RuntimeEdgeProvider` 提供：
  1. **Phase 1 — `CrossFunctionBridge`**：有摘要时 O(1) 查表（ArgToParam / ReturnToCall）。
  2. **Phase 2 — runtime BFS join**：无摘要时的路径。
- `RuntimeEdgeProvider` 同时承载不应写入函数内 `dataflow_edges` 的框架状态桥。ArkTS 当前
  将 `AppStorage.set/setOrCreate(key, value)` 的 value 参数，以查询时 `StateFlow` 连接到
  同 key 的 `@StorageProp` / `@StorageLink` 字段读取及其外层调用参数。key 只做字符串引号
  与空白规范化，不执行常量/枚举求值；反向 `StorageLink` 写回、字段默认值初始化及进程边界
  暂不建模。该语义遵循官方 [AppStorage 状态模型](https://developer.huawei.com/consumer/cn/doc/harmonyos-guides/arkts-appstorage)。

**模式语义（强制）**

| 模式 | 是否有 summary phase | 跨函数主路径 |
|------|----------------------|--------------|
| **Full Index** | 有（成功 summary build 后 `SUMMARIES` bit） | Phase 1；无摘要表时不应依赖 Phase 2 冒充 full 能力——要求重建/重索引 |
| **Focus** | **无**（Focus 不跑全仓 summary） | **Phase 2 runtime BFS 是设计主路径**，不是“兼容兜底” |

禁止把 Phase 2 从 Focus 路径删除：会打断 Focus 下跨函数 trace。

## 8. Resolution 与 Graph 约束

- `ReferenceResolver` 只产生 resolved facts。
- `GraphBuilder` 从 resolved references、callsites、raw structural facts 生成 symbol-level edges。
- `GraphSnapshot` 对消费者不可变；刷新时 writer 在独占可变引用（`&mut self`）下通过 `replace_files_in_place` 做增量更新，或对大变更集重建完整 snapshot。
- 删除或修改文件时必须失效相关 references 和 edges。

**调用边仅限项目内部符号**：Atlas 只在 caller 和 callee 两端符号都已索引时创建调用边 (`Calls`/`Instantiates`/`Implements`)。外部包的引用（如 `import { useState } from 'react'`、`#include <stdio.h>` 中的 `printf`）因目标符号不在项目的 symbol table 中，不会产生边。具体机制：

1. **解析阶段**：外部符号在项目中无对应 symbol，reference 保持 unresolved
2. **边构建阶段**：`create_edges_for_reference` 通过 `find_symbol_by_id` 校验目标符号存在于 store；不存在则 `return Ok(edges)`（空 Vec）。source 符号（reference 的 enclosing function/class）不存在时间样跳过。

```text
项目内 foo()             → edge 创建 ✅
import { bar } from 'lodash'  → bar() 无 edge ❌
顶层表达式调用（无 enclosing 函数） → 无 edge ❌
```

置信度分层：

```text
1.00 compiler/LSP/SCIP exact, if future supported
0.95 same-scope exact / exact qualified name
0.90 import/package exact
0.80 framework convention / namespace proximity
0.70 same-file or same-package name match
0.60 project-wide exact name fallback
0.50 fuzzy / ambiguous fallback
<0.50 unresolved or speculative
```

约束：
- project-wide exact name fallback 记录为 `name_only`，不能伪装成 `fuzzy_match`。
- `fuzzy_match` 仅用于真实编辑距离 fallback。
- 1-2 字符短名不执行 project-wide edit-distance fallback；短名只能通过 scope、same-file、import 或 exact name 解析。

## 9. 语言能力边界

### 9.1 模型

```text
LanguageCapabilityProfile
  language
  capability_level       → None / Symbolic / DataflowLocal / DataflowInterproc
  supported_features     → 人类可读的 feature 名称列表
  unsupported_features
  known_limitations
  confidence_floor       → 0.0-1.0
  features               → FeatureMatrix（必填；类型安全的逐 feature 查询）
```

`features` 是能力门控的唯一权威。`supported_features` / `unsupported_features` 只是面向人类和 JSON 输出的镜像列表，必须与 `features` 保持一致，但运行时不得以字符串列表作为能力判断来源。

### 9.2 权威能力表

从代码 `capability.rs` 导出，与实现保持同步：

| Language | Level | CFG | Confidence | Interprocedural | Note |
|----------|-------|:---:|:---:|:---:|------|
| TypeScript | DataflowInterproc | ✓ | 0.60 | ✓ (ArgToParam + ReturnToCall) | Summary tables + CFG |
| JavaScript | DataflowInterproc | ✓ | 0.60 | ✓ (ArgToParam + ReturnToCall) | 共享 TS adapter |
| Python | DataflowInterproc | ✓ | 0.72 | ✓ (ArgToParam + ReturnToCall) | scope-chain-aware binding |
| Java | DataflowInterproc | ✓ | 0.75 | ✓ (ArgToParam + ReturnToCall) | |
| C | DataflowInterproc | ✓ | 0.73 | ✓ (ArgToParam + ReturnToCall) | 函数指针 limited depth 3 |
| C++ | DataflowInterproc | ✓ | 0.70 | ✓ (ArgToParam + ReturnToCall) | 模板/重载/ADL 不建模 |
| ArkTS | DataflowInterproc | ✗ | 0.60 | ✓ (ArgToParam + ReturnToCall + AppStorage StateFlow) | TS grammar + 等长 struct 归一化；trailing-block parse status 仍可能 partial；CFG 未实现 |
| Go | DataflowInterproc | ✓ | 0.78 | ✓ (ArgToParam + ReturnToCall) | 泛型未捕获 |
| C# | DataflowInterproc | ✓ | 0.72 | ✓ (ArgToParam + ReturnToCall) | `using_statement` CFG；partial classes 未合并 |
| Rust | DataflowInterproc | ✓ | 0.70 | ✓ (ArgToParam + ReturnToCall) | 宏/borrow 语义不建模 |
| PHP | DataflowInterproc | ✗ | 0.62 | ✓ (ArgToParam + ReturnToCall) | name-based binding；CFG 未实现 |
| Ruby | DataflowInterproc | ✓ | 0.65 | ✓ (ArgToParam + ReturnToCall) | block resource CFG；yield 仍为 best-effort |
| Kotlin | DataflowInterproc | ✓ | 0.67 | ✓ (ArgToParam + ReturnToCall) | branch/loop CFG；extension receiver binding 为 best-effort |
| Cangjie | DataflowInterproc | ✓ | 0.65 | ✓ (ArgToParam + ReturnToCall) | postfixExpression callSuffix |

约束：
- capability profile 属于 engine/analysis 边界；CLI/MCP/context 只能读取并展示。
- 每个查询结果必须携带实际使用的语言能力信息。
- 查询请求超出当前语言边界时，trace 内层返回 `partial_result + diagnostics`；非 trace MCP 外层返回终态 capability gap，不静默返回无法解释的空数组。
- 低置信度 fallback 必须带 `confidence`、`strategy` 和 `provenance`。

### 9.3 FeatureMatrix 能力门控

- `trace_variable`：门控 `local_dataflow.is_supported()`。
- `trace_callers`：门控 `call_graph.is_supported()`。
- `trace_point`：始终可用。
- `derive_capability_level()` 的升级条件：
  ```
  DataflowInterproc = local_dataflow + use_def + interprocedural_summaries
                 + returns_flow + call_arguments (all supported)
  DataflowLocal = local_dataflow + use_def (supported)
  Symbolic      = symbols + references (supported)
  ```

## 10. Extraction 实现

当前抽取层：
- `ParseWorkerPool` — 支持 max file size、panic isolation、结构化 `ExtractionError` 和 `IndexReport`。
- `SemanticBinder` — 统一填充 source/scope/binding。
- `LexicalBinder` + `DataFlowBuilder` — 词法绑定与数据流。
- `CfgBuilder` — 函数级 CFG；当前除 ArkTS、PHP 外的 12 种语言均在 capability profile 中声明支持。
- Golden test framework 覆盖 14 种语言。

符号源码范围是抽取事实，不由展示层猜测。函数/方法使用 enclosing callable
scope；C/C++ 的 class/struct/interface/enum 使用完整 defining scope（包括成员和闭合
delimiter），不能把 manifest 的声明起始行冒充完整定义。TUI、MCP `explore` 和
`symbol(includeCode=true)` 都只消费这一个范围事实。

旧数据库可能具有相同 content hash 但由旧抽取语义生成。Lazy structural cache 命中前
执行少量、可证明的不变量检查：call reference 的 owner 必须是 callable；C/C++ 一行
type range 若对应源码已经打开但未闭合定义，则不是完整 structural fact。违反不变量的
文件按需自愈重抽，而不是全库失效或在展示层拼接源码。只有无法从现有事实与当前源码
可靠判定的抽取语义变化，才应提升 schema/extractor revision 并要求重索引。

已知限制：
- CFG 是 tree-sitter 驱动的 best-effort 控制流，不等同于编译器 CFG；复杂异常、异步、标签跳转和语言特有控制结构的精度以 capability limitations 与 golden fixtures 为准。
- ArkTS 和 PHP 当前不声明 CFG 支持；其余语言已覆盖核心 branch/loop body traversal，部分语言另有 resource/context 结构覆盖。
- 全量抽取 worker 仍没有线程隔离式硬 timeout；查询时 Focus lazy structural 通过
  `CancelCheck` 检查点受 `FocusWindow` 总预算约束。

### 10.1 查询时 Focus 架构（按需物化为内部机制）

查询时路径的**产品语义是 Focus**：在 Index/FullCache 不可用时，围绕用户意图建立热点与闭包，有选择地物化局部 facts，使闭包内分析体验接近「该邻域已被全仓索引」。manifest / resolution_symbols / structural / dataflow 多层与 extraction state、job tracking、query resume、investigation 支撑可观测的渐进分析。

**Index** 仍是简单预物化路径；二者共享事实底座与抽取语义，差异在**何时、对多大范围**支付物化成本。

#### 10.1.1 Layer 层次结构

提取精度按层（layer）建模，从最轻量到最完整：

| Layer | 说明 |
|-------|------|
| `manifest` | 仅顶层符号（type/function/class 声明），无引用、无 scope。通过 `--analysis manifest` 产生。 |
| `resolution_symbols` | 最小符号层，仅供跨文件引用解析使用。包含 symbols、imports、scopes，不包含 references、callsites、dataflow、raw_edges。 |
| `structural` | 完整符号、引用、scope、边。通过 `--analysis structural` 或 Focus 按需 structural 物化产生。 |
| `dataflow` | 所有 structural 事实 + per-function dataflow/CFG。通过 `--analysis full` 或 Focus 按需 dataflow 物化产生。 |

Layer 通过 `SymbolDef.layer` 和 `extraction_state.layer` 字段标识。

#### 10.1.2 Extraction job 活跃边界

按需 extraction（Focus materialize）的 in-flight 工作通过 `extraction_jobs` 表追踪，确保可观测性和并发去重：

```
queued → building → complete
                  → failed
```

- **queued**: 作业已注册但尚未开始。
- **building**: 正在执行提取。
- **complete**: 提取成功完成。
- **failed**: 提取失败（`error_msg` 记录原因）。

Job ID 基于时间戳生成（`extract_{microsecond_hex}`）。同一 `(file_id, unit_id, layer)` 在 `queued`/`building` 状态下有且仅有一条活跃记录；文件级 job 的 `unit_id` 为 `NULL`。并发请求通过 claim API 的 dedup 语义使用同一 job_id。

`extraction_jobs` 不是长期审计日志。文件级 structural rebuild 会原子替换 `files`
行，相关 job 可能随 FK cascade 被清理；完成事实以 fresh `extraction_state`
和实际 facts 为准。公开查询只依赖 active job 是否存在、pending job id、以及
`analysis.retry_after_ms` / 终态 `gaps`，不得把缺少历史 complete job 解释为仍在 building。

Job tracking 表结构：参见 `db::schema::SCHEMA_DDL` 中的 `extraction_jobs` 表。

#### 10.1.3 内部精度模型

引擎内部使用 `AnswerQuality`（**AnswerQuality**）`{ coverage, confidence }`，把“覆盖范围”和“语义确定性”分开建模（L4，见 §1.1）：

- `CoverageTier`：`RepoComplete`、`ClosureComplete`、`Boundary`、`Partial`、`Manifest`。
- `SemanticConfidence`：`Low`、`Medium`、`High`、`Certain`。

`AnswerQuality` 只参与 Focus/closure 的调度、终态和 gap 推导，不进入 MCP 公共响应。Agent 只消费 `analysis.basis`、可选 `analysis.retry_after_ms`、可选 `coverage_counts` 与终态 `gaps`。读路径控制面使用 `AccessStrategy`（`FullCache` | `Focus`）。

#### 10.1.4 In-flight 一致性

- **去重**: `extraction_jobs` 表确保同一 file+unit+layer 不会并行构建两次。
- **读写一致性**: 每个 handler 在触发 lazy extraction 后，在自己的写事务中可见刚写的数据；读操作通过 `StoreReader`（独立只读连接）访问。
- **Delta graph refresh**: lazy structural 写入后，通过 incremental refresh 或必要时完整 snapshot rebuild，确保图查询能看到新边。

#### 10.1.5 Closure 与 Linux 增强边界

- Focus closure 同时记录结构化文件集合和相关符号前沿。symbol seed 必须保留精确
  `SymbolId`/`file_id`；扩展只能从 seed 或前一轮新发现的相关符号出发，不能因为一个
  文件进入 closure 就把该文件所有 peer 的调用和类型关系都加入前沿。
- `ClosurePlanner` 基于 import/include 图计算解析边界，确保被引用文件的
  `resolution_symbols` 层先于主文件的 scoped resolution 构建。依赖文件默认不进入
  structural closure；只有 call/type 关系证明它与查询相关时才升级为 structural。
- `resolution_symbols` 层实现: 轻量提取模式，产出 symbols + imports + scopes（无 references/callsites/dataflow/raw_edges），供跨文件引用解析使用。
- 每轮有限不动点按“相关 structural facts → dependency resolution symbols → 对当前
  bounded closure 重新 scoped resolution → call/type expansion”推进。重新解析已有文件
  是必要步骤，因为新加入的 resolution symbols 可能改变上一轮 unresolved reference。
- `calls`、`path`、`impact` 的前台阶段只物化精确 seed 所在文件并返回当前可证明的图；
  请求深度决定可恢复的后台不动点轮数，并受各工具公开上限和文件/时间预算共同约束。
  这避免大文件邻居扩展占住首个响应，同时不把 seed-only 伪装成完整结果。incoming call graph
  在目标文件仍冷时允许使用 bounded reference
  candidate discovery 找到潜在 caller 文件，再以真实抽取和 scoped resolution 验证；
  candidate 命中本身不是 graph edge。
- Linux 增强：C 的 syscall 宏、EXPORT_SYMBOL、initcall 等在 `extract_file_with_mode`
  成功返回前经共享 `apply_post_extract_hooks` 增强；Index 与 Focus structural ensure 共用同一 hook。

#### 10.1.6 Index 与 Focus materialize 能力边界

- **Index scope**：`--include` / `--scope` / `--exclude` 限制预物化范围。
- **Manifest**：`ExtractionMode::Manifest` 仅顶层符号；供 candidate / Focus 冷启动。
- **Focus structural materialize**：`LazyStructuralService` + `CandidateProvider`，归属 `FocusMaterialize`。
- **Focus dataflow materialize**：`LazyDataflowService` + `LazyWindow` / budget，归属 `FocusMaterialize`。
- **可观测**：`FactCoverage`、`QuerySnapshot` / `resume_query`、`tasks`、session `Investigation`；MCP 公共面见 `analysis` / `gaps` / `query_id`。

#### 10.1.7 extraction 状态与任务

文件级 / unit 级状态与进行中任务由 `extraction_state` / `extraction_jobs` 表达：

- **完成状态**：文件级 `unit_id IS NULL` 且 hash 匹配 `files.content_hash`；unit dataflow cache 用 `unit_id IS NOT NULL`。
- **进行中**：on-demand structural/dataflow 必须 claim job；已有 active job 返回 pending id。dataflow 用 unit-scoped job key。
- **MCP 可观测**：`status` 由 fresh layer 推导 catalog tier；Agent 只读 public `analysis`，不读 raw job 字段。
- **Search**：先 store-backed 查询，再对候选定向 structural ensure；大 scope 不同步全量 structural。

#### 10.1.8 可中断提取

- **`CancelCheck`**（`extraction/cancel.rs`）：`fn is_cancelled(&self) -> bool`；无取消需求传 `&()`。
- **`extract_file_with_mode`**：唯一 extraction 入口；须显式 `ExtractionMode` + `CancelCheck`；检查点 CP1–CP4（parse 前、符号后、引用后、导入/作用域后）。
- **`collect_captures`**：约每 100 次迭代检查取消；取消返回 typed failure，不写截断 partial 为成功。
- **取消分类**：`ExtractionFailureKind::Cancelled` / `FailureCategory::Cancelled`；禁止字符串匹配推断取消。
- **`ReindexOutcome`**（`lazy_structural.rs`）：`Built` / `Cancelled` 枚举，在 DB 写入前（CP5）和 extraction 调用前（CP6）检查。`Cancelled` 设置 `budget_exceeded=true` 但不计入 `files_built`
- **`LazyBudget`** 实现 `CancelCheck`（`is_cancelled = cancelled || time_exceeded`）。`can_continue()` 超时时自动调用 `cancel()`

此设计不修改 tree-sitter C FFI，不引入 signal，不增加线程。Cancellation 是正常降级路径，产生 precision 降级而非 MCP tool error。

#### 10.1.9 Query Resume 与 Investigation

响应信封采用三态终局模型（见 §10.1.10），Agent 通过 `analysis.retry_after_ms` 和顶层 `gaps` 即可判断结果状态。Tool 响应的 coverage/missing 语义通过 `analysis.basis` 和 `gaps[].reason` 承载，可提升空间通过 `retry_after_ms` 表达。废弃的 `analysis_contract` 结构体已移除。

`query_id` 是 MCP 层概念，不复用 extraction job id。查询快照保存在 MCP session 内存中，创建后 TTL 为 5 分钟；快照保存原 tool 参数及本次 `FocusResult` 的 live `JobTracker`。`resume_query(query_id)` 复用该 focus 状态重放查询，不重新调度 closure，返回完整增强结果而不是 diff。MCP server 重启后 query snapshot 丢失。

语义图查询重放前必须把快照中的前台 `built_files` 和 `JobTracker` 记录的后台
materialized files 一并加入增量 graph refresh queue。后台 focus 可能在初始响应后替换
同一文件的最终 facts；仅比较计数/秒级时间戳的 index signature 不能可靠发现这种等量
替换。SQLite 中已有而 `GraphSnapshot` 尚不可见，不算 refinement 完成。

`Investigation` 是 MCP session 级隐式调查上下文，不提供用户可见的 create/close API。分析类工具会根据 symbol、position 或 field focus 更新 active investigation，并把相关文件/符号和期望能力传给 lazy 调度器。TTL 同样为 5 分钟。

#### 10.1.10 统一对外分析认知界面

MCP 分析响应采用**三态终局模型**，Agent 通过 `analysis.retry_after_ms` 和顶层 `gaps`
两个信号即可判断结果状态。模型保证终态必然可达——不存在永久 `building` / `wait` 状态。

```
状态 1 — 非终态（后台仍在运行）
{
  "result": {...},
  "analysis": {
    "scope": "local",
    "summary": "Focus analysis still expanding: 2 pending job(s) remaining.",
    "basis": ["manifest", "structural"],
    "retry_after_ms": 10000
  }
}
Agent: schedule_poll(query_id, retry_after_ms) → resume_query

状态 2 — 终态：完整
{
  "result": {...},
  "analysis": {
    "scope": "local",
    "summary": "Focus analysis complete: all background jobs have finished.",
    "basis": ["manifest", "structural"]
  }
}
Agent: use_with_confidence(result)

状态 3 — 终态：有永久缺口
{
  "result": {...},
  "analysis": {...},
  "gaps": [
    {"scope": "function_qname", "reason": "no_dataflow", "detail": "dataflow facts not yet available"}
  ]
}
Agent: use_with_caution(result) 或尝试其他查询策略
```

Agent 消费伪代码（唯一入口）：

```
if resp.analysis?.retry_after_ms:
    schedule_poll(query_id, retry_after_ms)   # 非终态：等待后重试
elif resp.gaps:
    use_with_caution(resp.result)              # 终态有缺口：谨慎使用
else:
    use_with_confidence(resp.result)           # 终态完整：直接使用
```

**`gaps` 字段结构**：`[{scope, reason, detail}]`，每个 gap 描述一个分析缺口：
- `scope`：缺失范围（符号限定名或文件路径）
- `reason`：机器可读原因码（例如 `no_dataflow`、`no_cfg`、`closure_boundary`、`budget_exhausted`、`symbol_resolution`）
- `detail`：人类可读补充说明

`gaps` 仅在终态响应中出现。非终态不暴露瞬时缺口，避免 Agent 误判为终态而早停。

**内部状态 → 公开信号的映射**：

| 引擎内部状态 | `retry_after_ms` | `gaps` |
|-------------|------------------|--------|
| 索引完整、数据充足 | 不存在 | 不存在 |
| 后台任务运行中 | 存在（由 JobTracker.eta_ms() 计算） | 不存在（瞬时缺口不暴露） |
| 所有任务完成，有永久缺口 | 不存在 | 存在 |
| 预算/语言能力/配置限制 | 不存在 | 存在 |

`retry_after_ms` 的 ETA 由 `JobTracker` 基于已完成闭包的平均构建时间外推：
`eta_ms = avg_completed_duration × pending_count`，基线值 5000ms，上限 60000ms。

**约束**：

- 引擎层概念（`AnswerQuality`、`AccessStrategy` 原始枚举、closure ID、调度器优先级）不进入 MCP 公共响应。
- 非 trace 工具的公共信封用 `retry_after_ms` + `gaps` 表达进行中/缺口；trace 内层 frozen contract 可含自有 `partial_result`。
- 不暴露 `background_refinement` 公共字段。

#### 10.1.11 Focus 与 materialize；对照 Index

**对外叙事只有 Index（预物化）与 Focus（查询时）。**  
按需写库是 Focus 的 materialize 实现；机制类型可名 `Lazy*`，不是并列产品。

| | Index | Focus |
|--|-------|-------|
| 角色 | 简单、通用预物化 | 意图驱动的局部加强 |
| 入口 | `atlas index` / IndexPipeline / sync | MCP `FocusRuntime` + `FocusMaterialize` |
| 读策略 | `AccessStrategy::FullCache` | `AccessStrategy::Focus` |
| 体验目标 | 全仓/scope 缓存可读 | 闭包内可用性 ≈ Index 同邻域 |
| 不变量 | 不依赖 Focus 控制面 | 共用 extract/post-extract；单一 materialize 配置；邻域 facts 切片可对拍 |

- **控制面** `FocusRuntime`：构建范围、顺序、closure 可见性、analysis/retry/gaps。Handler 只产 `QueryIntent`。
- **符号解析（短名 / 限定名）**：plain string 先 exact `qualified_name`，未命中再按 **simple name**（路径最后一段，分隔符 `.` / `:` / `\`）查 `symbols.name`。多命中返回 `Ambiguous`，候选带完整 `qualified_name` 与 `symbol_ref`（含 file/line），引导客户端下次用精确 qname 消歧；`calls` Aggregate 策略可对多 id 并集 callers/callees。C++/PHP 限定调用抽取只 capture 末段 name（如 `CertUtils::GetDev` → ref.name=`GetDev`，`text`=全文，`receiver`=前缀），以便 resolution 建 `Calls` 边（改 query 后需 re-index）。嵌套 `A::B::C` 的 `text`/`receiver` 取最外层 `qualified_identifier` 跨度。
- **MCP orchestration**：`contract_for(name, args)` 决定 `ToolContract`；`call_tool` 按 contract 走 `dispatch_*` 再进 handler。**Analysis 工具**（`lifecycle` / `branch_diff` / `impact(semantic=true)`）的编排所有权在 `AnalysisRuntime`（见下），不是 handler 内联 service。
- **Handler 纯度（DEBT-8，ratchet 完成）**：`handler_purity` 源码扫描双层守卫——(1) 禁止 engine 直点名（`FieldLifecycleEngine::` / `BranchDiffEngine::` 等）；(2) 禁止 analysis tool handler 内 orchestration 模式（`find_cfg_*` / `find_data_nodes_*` / `compose_effects(` / `CfgGraph::build` / `CppOwnershipRules::load_for` / runtime helper 拼装等）。allowlist **只缩不涨**；残量条目必须仍有真实 FORBIDDEN 命中。god-router 已不再直接 `focus_runtime.lock()`（统一走 `QueryRuntime` 委托：`enqueue_file_focus_warm` / `focus_materialize_*`），annotation 测试 seed 走 `overlay_runtime`。**唯一残量**：`active_project.rs` 的 `FocusMaterialize::open`--这是 project-open **工厂**（构造期一次焊死 materialize，非 per-request 编排），factory != handler orchestration，记为合法例外。残量上限 `assert!(allowlist.len() <= 1)`。`graph.rs` 只选择 impact 子图目标并调用 `AnalysisRuntime::run_semantic_impact`。
- **Materialize** `FocusMaterialize`：ensure、budget、job 去重、rebuilder。唯一构造 `open`；`FocusRuntime` 构造必填 materialize；prepare 不静默再 `open`。MCP 禁止旁路未配置 dataflow；禁止热路径 `Engine::from_store` 并立 materialize 第二栈。
- **跨进程写互斥**：CLI `atlas index`/`sync` 持 `FileLock`（`exclusive_lock_pid`）。Focus structural/dataflow **写前** 走 `Store::reject_if_exclusive_lock_held_by_other`（filesync 与 dataflow loader 共用诊断源）：若其他 live PID 持锁则 **立即 reject**（无 wait/queue），诊断码 `cli_index_lock_held` + suggested_action。
- **`AnalysisRuntime`（真 dispatcher，非改名 facade）**：共享 materialize 上的 ensure **与** 全链路 analysis 编排——能力门控（lifecycle 仅 C/C++）、dataflow ensure/I/O、`compose_effects`、ownership rules 加载、`FieldLifecycleEngine` / `BranchDiffEngine` 调用。公开入口仅为完整用例：`run_lifecycle` / `run_branch_diff` / `run_semantic_impact`；store/composition/engine helper 为 runtime 私有。C/C++ 持久化 `alloc_fn` / `free_fn` / `cleanup_fn` 合并进该语言既有 `ResourceOpConfig` 后参与同一次 `compose_effects`，不得替换并丢失默认 matcher/implicit-cleanup；其他语言使用各自默认 config。handler 只做参数/符号/图目标准备与 envelope 渲染。不是第二 materialize 配置。
- `FocusRuntime` 是 MCP 查询时唯一控制入口。
- `SemanticFunction` intent：只保证目标函数文件的 structural/dataflow/CFG，不排 call/type expansion。
- Focus resolution 写 closure-scoped `reference_resolutions` 与 scoped graph overlay；全局 `references.resolved_*` 与 repo-wide `symbol_edges` 仅由 full-index / shared pipeline 更新。
- 内部质量用 `AnswerQuality`；不进 public MCP contract。
- Closure expansion：策略顺序去重；import/include 可见性查询先依赖后 call graph；超预算按容量截断并 `budget_exhausted` gap，不得整批拒绝成 seed-only。
- 仅 `FocusResult.pending_closure_ids` 中可追踪的后台 closure 可排入；前台已建文件不得再隐藏 Recent prewarm。
- `closure_coverage` / `reference_resolutions` / `symbol_edge_candidates` 为临时 control-plane facts；新 session 清表；同 session 成功物化后只保留最近 16 个 committed closure。
- 已有 inventory 或源码事实时 bootstrap 只标 ready，不全仓后台扫；resolver fallback 用 `symbols.name`，不建 project-wide `GlobalSymbolIndex`。
- `JobTracker` 记录 closure 终态与耗时；失败必退出 pending 并映射 `background_refinement_failed` gap。经 `FocusResult.job_tracker` 交给 MCP 判 retry/终态。
- `EnsureStructuralResult` 只把实际 built/cached 文件计入 closure。抽取失败记录
  `extraction_failed` gap；取消或预算截断记录 budget gap；`AlreadyBuilding`
  记录为 retryable pending extraction job，并通过 `analysis.retry_after_ms`
  收敛，不作为终态 `gaps` 暴露。请求过但没有事实的文件不能计入 coverage，也不能提升为
  `ClosureComplete/High`。
- `ClosureComplete/High` 只适用于非空且无 gap 的结构化闭包；只有 manifest、只有
  resolution symbols、空闭包或存在 gap 时必须诚实降级。
- Focus 是内部机制，不是 public response surface。默认 MCP 响应和
  `atlas_status` 不暴露 `focus`、closure id、scheduler priority、
  bootstrap tier 或 focus-specific pending queue；只暴露公开语义的
  `retry_after_ms`、`coverage_counts`、`gaps`。

> **乔布斯语录**：本实体为解决 4 环无限等待链路而引入——`precision` 硬编码致终态不可达、`pending_closure_ids` 永不为空、前台闭包 ID 污染待处理列表、调度器无完成通知。修复后的核心不变式：`mark_done` 在构建成功时立即调用，前台 ID 不进入 pending，终态由 `are_all_done` 判定，`eta_ms` 提供自适应重试间隔。

### 内容哈希一致性

当 `upsert_resolution_symbols` 检测到磁盘上的文件内容自上次 `files` 行写入以来已经变更（内容哈希不同），它会在同一事务中原子性地更新 `files.content_hash`。所有之前存在的更丰富层（structural、dataflow）变为过期状态，因为它们记录的 layer hash 不再匹配更新后的 file hash。在下次 lazy 访问时它们将从当前内容重建。

此"安全更新"策略保证渐进式富化永不会悄悄提供过期数据，代价是可能需要重建过期的层。
Content hash 只证明源码未变，不证明抽取器语义未变；§10 的 structural 不变量检查补足
可局部识别的旧事实。不得把 hash 相同直接等同于所有历史抽取事实仍语义有效。

### 10.2 共享索引管线

`filesync::IndexPipeline` 是入口无关的索引主链路，负责：

```text
discover files
  → compute dirty set (optional, caller-controlled)
  → clean stale facts
  → extract FileFacts
  → optional reference resolution
  → optional graph edge build
```

约束：
- CLI、MCP、sync 入口只负责参数解释、锁、UI/进度、后台任务和错误展示。
- 共享管线不直接输出终端文本、不依赖 MCP transport，也不安装 Ctrl+C handler。
- `ExtractionMode::Manifest` 在抽取后停止；`Structural` / `Full` 继续执行 resolution 和 graph build。
- full-index 共享管线必须执行 dirty/deleted-file 检测并清理 DB 中已不存在的文件；仅清理本次 discover 到的文件不足以保证索引权威性。
- `Full` 共享管线必须在 graph/annotation 阶段后构建 persistent function summaries，并把 summary capability 反映到持久化状态。
- 新增索引阶段时优先进入共享管线，再由入口层决定是否暴露配置。
- `filesync::build_dirty_set_for_mode` 是 `IndexPipeline` 的 HashCheck 边界；CLI/MCP/TUI 不直接实现 DB hash diff 或 capability upgrade 判断。
- HashCheck 的 clean 定义是“content hash 相同 + fresh complete file-level `extraction_state` 覆盖本次 `ExtractionMode` 所需 capability”。hash 相同但缺目标 capability 的文件必须进入 dirty set，以支持 manifest → structural → full 的无源码变更升级。
- 目标 capability 映射为：`Manifest`/`ResolutionSymbols` 需要 manifest，`Structural` 需要 structural，`Full` 需要 dataflow；更高层 capability 可满足低层要求。
- `project_metadata` 中 `last_index_time`、`last_sync_time` 等可选键不存在时表示未知/尚未发生，不是错误，不得产生 warning；只有表/列/SQL 等真实查询失败才记录 warning/error。
- 当前版本未发布，不保留旧 schema 运行时 fallback；如果 schema contract 改变，应更新 DDL 和调用方，并要求重新建库/重索引。
- `filesync::clean_stale_file_*` 是 stale facts 清理边界；所有入口必须先清理 incoming refs 和 outgoing edges，再删除旧 facts。
- path alias 配置文件集合由 `resolution::PATH_ALIAS_CONFIG_FILES` 定义，当前为 `tsconfig.json` 和 `jsconfig.json`；检测、提交 hash、加载 resolver 必须使用同一来源。
- 此契约（入口管参数/锁/UI/进度，管线管索引机制）是核心架构不变式，由 `pipeline_equivalence` 集成测试验证：同一项目通过不同入口索引必须产生相同 DB 状态（files/symbols/edges/summaries）。`IndexPipeline`（全量）与 `IncrementalPipeline`（增量）是仅有的 DB 变更编排路径；CLI/TUI 不得复制 phase 逻辑。

### 10.3 引擎服务层

入口通过服务层编排能力，不直接组合低层 API：

- `ScopedSearchService`：scope 感知搜索 + 定向 lazy structural。
- Tracing 经 `Engine` facade；`Engine` 负责触发 lazy dataflow，raw `TraceEngine` 仅消费已有 facts。
- 约束：入口组合服务；服务组合 `Store`、extraction、graph 等；入口绝不对低层 API 做 ad-hoc 组合。

#### TUI 查询工作台

TUI 继续使用 Ratatui；当前问题域不需要第二套终端框架。其边界分为两类：

- 高频交互由 TUI 原生状态机承担：symbol search、详情 tabs、caller trace、选择和滚动。
- 低频分析通过 `:` command palette 进入既有 `atlas_mcp::tools::ToolRouter`：
  `symbol`、`calls`、`explore`、`impact`、`path`、`trace`、
  `file_dependencies`、`lifecycle`、`branch_diff`、`domain_rules`、
  `fp_dispatches`、`tasks`、`resume_query`。当前选择的 qualified name / file path 由入口层注入；
  其余参数通过 typed field form 填写。枚举和布尔参数循环选择，数值和文本参数在提交前校验。
  `trace kind`、`domain_rules action`、`fp_dispatches action` 等 discriminator 同时驱动字段可见性、
  动态必填、键盘导航和最终参数生成；四者不得各自维护分支规则。TUI 不要求用户手写 MCP JSON。

TUI 的后台工作在既有单 worker `JobManager` 中执行；`JobManager` 持有一个
session-persistent `ToolRouter`，使 `query_id`、`tasks` 和 `resume_query` 在多次命令间
保持有效。TUI 从最新响应读取顶层 `query_id`，自动填入 `tasks` / `resume_query` 表单。
主线程只处理按键、取消、状态切换和渲染。`tui::tool_result` 是唯一的结果展示投影边界：

- 原生 symbol search 先读取 SQLite facts；图未就绪时使用空图提供中性的 degree 信号，
  不在 UI 线程构建全量 snapshot。精确名称、类型、语言和路径排序不受影响。
- 首次打开 graph-backed detail 时提交 `LoadGraph` job。worker 从 Store 构建不可变
  `GraphEngine`，主线程只安装 snapshot 并创建轻量 `ContextBuilder`。lazy 写入只标记
  snapshot stale；下一次 detail 复用相同后台 reload 路径。
- `GraphSession` 不保留同步 lazy-init/refresh 入口，避免以后再次把大型 snapshot 构建
  放回按键处理路径。
- 新 job 替换旧 worker 时只设置 cooperative cancel 并 detach 原 `JoinHandle`，不得在
  `submit()`/按键路径固定 sleep 等待取消。

- 默认视图把 subject/source/path/steps/hops/file groups 等代码事实置前，调用与关系证据次之，
  文件 inventory 和 recommendations 置后；符号、import、domain rule、function-pointer dispatch、
  task 使用稳定业务字段压缩为人类可扫描的行。
- `analysis`、`capability`、confidence、coverage、partial/truncated、diagnostics/gaps 等公共元数据
  进入自适应 HUD 或诊断区。HUD 只消费 handler 明确返回的字段，不推断 precision、coverage、
  完整性或置信度；缺失字段不生成“No metadata”之类占位噪声。
- 未识别的非元数据字段必须递归保留在 facts 视图，不能因为当前 TUI 尚不了解新字段而静默丢失。
- `r` 可随时切换到未经展示投影修改的 pretty JSON/text；raw response 是审计与前向兼容后门。

投影只改变呈现，不重建 analysis envelope，也不改变 lazy/focus、终态或错误语义。这样 TUI
和 MCP 对相同查询仍使用同一 handler 事实，同时人类不需要阅读 wire-format JSON。

TUI 不暴露 `project` palette command：TUI 生命周期绑定启动时的单个本地项目，切换
router project 会让原生 search/context session 与 tool session 分裂。`search` 已由原生
交互视图承担。`atlas-mcp` 因而是
`atlas-cli` 的常规库依赖；`mcp` Cargo feature 只控制 stdio transport 所需的 Tokio 和
`atlas mcp` 子命令。

### 10.4 长操作进度与取消

跨 CLI/TUI/MCP 的长操作（index、sync、focus extraction、trace）使用统一
取消和降级原则：

- `ProgressSink` trait：入口注入终端进度、MCP notification 或 no-op。
- `CancelToken`：前台/后台均可中断执行，取消是正常降级路径。
- CLI/TUI 的显式 `index` / `sync` 可以是长操作；MCP 不暴露 `index`，也不
  暴露 waitable task API。
- MCP 的 `project(action="open")` 只同步激活项目，不扫描全树、不索引。
- MCP scoped 查询触发 focus/lazy materialization；响应通过
  `analysis.retry_after_ms`、`gaps` 和 coverage 字段表达当前结果
  是否完整可用。
- `tasks` 仅用于观测当前 session 的 focus/lazy 活动；`resume_query`
  通过 `query_id` 重放最近查询，不能等待任意后台 task。

普通分析响应只解释本次结果；全局项目状态通过 `project(status)` 查询。

分析类 tool response 只有在后台工作和本次结果直接相关时才标记非终态：
- 当前响应处于非终态（`analysis.retry_after_ms` 存在），且后台工作会改变本次
  查询的质量或可用性。

```text
analysis response
  analysis               required for analysis tools
    scope                repo | local | file | symbol
    summary              当前可用事实和限制的短说明
    basis                使用的数据源（manifest, structural, dataflow, cfg, domain_rules 等）
    retry_after_ms       可选；存在表示非终态，Agent 应在此毫秒后轮询 resume_query
  gaps                   [{scope, reason, detail}]；可选，仅终态响应出现
  query_id               MCP 层查询标识符，用于 resume_query 重放
  coverage_counts        optional；公开 coverage label 的数量分布（非终态 + 终态均可）
```

**三态终局规则**：

| 响应状态 | `analysis.retry_after_ms` | 顶层 `gaps` | Agent 动作 |
|---------|--------------------------|-------------|-----------|
| 非终态（后台运行中） | 存在（`eta_ms` 计算值） | 不存在 | `schedule_poll(query_id, retry_after_ms)` |
| 终态—完整 | 不存在 | 不存在 | `use_with_confidence(result)` |
| 终态—永久缺口 | 不存在 | 存在 | `use_with_caution(result)` |

**终态判定**：由 `FocusResult` 中的 tracked closure pending 与 raw extraction pending
共同判定；任一仍 active 时响应保持 `analysis.retry_after_ms`。
终态保证可达——`JobTracker` 在 `FocusScheduler::process_detached_job` 每个闭包构建
成功后调用 `mark_done(closure_id)`，失败后调用 `mark_failed(closure_id, reason)`；
`FocusRuntime::prepare()` 在前台闭包完成后立即标记终态，前台闭包 ID 不进入
`pending_closure_ids`。两种后台终态都保留已物化文件供 graph refresh 使用。

### 10.5 架构收敛约束

清理目标不是单纯减少行数，而是把重复实现压回稳定边界。后续改动必须遵守以下模式：

- **先删除，再抽象**：零调用代码、过期别名、错误的 `#[allow(dead_code)]` 和未接入主路径的 facade 必须直接删除。只有当多个调用点共享同一不变式时才新增 helper、trait 或 struct；不得为了“参数成组出现”创建没有行为边界的状态对象。
- **入口层只做编排**：CLI、TUI、MCP 只解释参数、处理锁、进度、后台任务和用户可见错误。dirty check、stale cleanup、capability upgrade、precision downgrade guard、resolution、graph build 和 summary build 都必须走 engine/filesync/service 层的共享入口。
- **抽取层 helper 只承载机械一致性**：`languages::shared` 可以统一 `TextRange`、deterministic ID、`ScopeDef`、`BindingDef`、`ReferenceUse`、常见 `DataNode` 默认字段和 call-expression 查找。语言语义差异、特殊 AST 形状、return/callsite/field 规则必须留在各语言 adapter；禁止回到大型 `GenericExtractor`。
- **trait 默认实现只表达真正相同的规则**：如 `LanguageRuleKinds::validate_rule` 这类跨语言完全一致的校验可以进入 trait default；只要某语言的 rule kind、pattern、metadata 或展示名语义不同，就必须在 registry 中显式覆盖，而不是在默认实现里堆条件分支。
- **MCP analysis envelope 只有一个构建路径**：触发 lazy structural/dataflow、focus refinement 的 tool 响应必须通过 `AnalysisEnvelope` 等共享 builder 注入 `analysis`（含 `scope`/`summary`/`basis`/`retry_after_ms`）、`coverage_counts`、`gaps`（GapRecord 数组）、`query_id` 和 `QuerySnapshot`。`precision_tier`、`hint`、`lazy_diagnostics`、`partial_result`、`background_refinement`、`analysis.unit`、`analysis.coverage`、`analysis.missing`、`analysis.state`、`analysis.next_action` 等字段不应出现在公共响应中；可操作建议进入稳定的 `error` 或 `message` 字段。需要保留的低层诊断只能进入内部 debug 日志或显式 debug-only 工具。Graph、trace、search、context handler 不得手写同一 envelope，以免字段、status 或 retry 语义漂移。
- **public facade 改造以目标 API 为准**：快速原型期允许 breaking change。`atlas-engine` 顶层 supported API 由高层入口及其可命名的参数/返回类型闭包组成；不得 re-export `phase_*`、dirty/cleanup、planner workset、parser pool、summary store 等机制入口，也不得为了旧调用方式保留 wrapper、别名或过渡 API。当前 `analysis`、`trace`、`dossier`、`rule_engine` 和 Focus 控制模块因 workspace sibling 消费仍为普通 `pub`，归类为 workspace-internal、不承诺外部稳定；后续只能通过迁移完整 use case 的所有权来收紧，不能用 facade `pub(crate)` 伪造跨 crate 可见性。
- **测试支撑 API 不等同于死代码**：仅测试使用的构造器或 provider 注入点必须通过 `pub(crate)`、`#[cfg(test)]` 或注释明确用途；不能因为生产路径零调用就删除，也不能用无理由的 `#[allow(dead_code)]` 掩盖。
- **policy module 可以优先于 policy struct**：当规则只是一组纯函数和一个 guard（例如 index precision downgrade）时，保持自由函数模块更清晰。只有当对象需要携带跨入口生命周期、统一日志/遥测、或多条规则共同依赖的状态时，才引入 `Policy` struct。

### 10.6 Symbol Selector 解析引擎

**位置**：`crates/atlas-engine/src/symbol_selector.rs` — engine 层模块，CLI/TUI/MCP 均可复用。MCP 通过 `crates/atlas-mcp/src/tools/symbol_selector.rs`（39 行薄封装）委托调用。

**核心类型**：

| 类型 | 职责 |
|------|------|
| `SymbolSelector` | 结构化符号选择器，qualified_name 必填，其余可选 |
| `SymbolInput` | `Name(String)` 或 `Selector(SymbolSelector)` 的并集 |
| `SymbolResolution` | 解析结果：`Single` / `Ambiguous` / `NotFound` |
| `SymbolResolutionPolicy` | 歧义处理策略：`UniqueOrCandidates` / `Aggregate` / `BestEffortSingle` |
| `ResolvedSymbol` | 实际命中符号，**始终返回 DB 真实值**，不透过用户输入 |
| `ScoredCandidate` | 带分数的候选，含 `symbol_ref` 可跨工具复用 |

**容错计分优先级瀑布**：

计分优先级（硬编码常量，不可调参）：

| 优先级 | 字段 | 精确得分 | 模糊得分 | 说明 |
|--------|------|---------|---------|------|
| P1 | qualified_name | +10,000 | — | 必填，所有候选基础分 |
| P2 | file_path | +3,000 (exact) | +2,000 (suffix) / +1,200 (basename) / ≤1,000 (fuzzy) | 后缀/段重叠匹配，不使用编辑距离 |
| P3 | line | +1,200 (delta=0) | +800 (≤2) / +500 (≤10) / +200 (≤50) | 容错排序，**不使用负分** |
| P4 | kind | +200 | 0 | 弱 tiebreaker |
| P5 | language | +100 | 0 | 最弱信号 |

**核心不变式**：

- **不惩罚错误**：所有计分均为正向加成。`file_path` 写错、`line` 错位不会降低正确候选的排名——只是不会加分。
- **唯一性阈值**：`UniqueOrCandidates` 策略要求第 1 名与第 2 名分差 ≥ 400（等于 `SCORE_LINE_EXACT - SCORE_LINE_STRONG`）才接受为 `Single`。`kind`（200）和 `language`（100）单独不能强制唯一选择。
- **始终返回实际值**：`ResolvedSymbol.file_path` 和 `ResolvedSymbol.line` 来自 DB 事实，不是用户输入。当用户提供的 `file_path`/`line` 与实际不符时，记录在 `match_info.ignored_mismatches` 中而不是修改输出。
- **聚合上限**：`MAX_AGGREGATION_CANDIDATES = 50`。超出时返回 `partial_selector: true`，要求用户细化选择器。
- **SymbolId 内部性**：解析器输出 `SymbolId` 供内部使用，但外部 JSON 响应中不出现 hex SymbolId。`ScoredCandidate.symbol_ref` 是自包含的 `SymbolSelector`，可直接作为下一个查询的输入。

**解析策略**：

| 策略 | 多候选行为 | 使用工具 |
|------|-----------|---------|
| `UniqueOrCandidates` | 分差 ≥ 400 → `Single`；< 400 → `Ambiguous` 返回候选列表 | `symbol detail`, `explore`, `context` |
| `Aggregate` | 始终返回所有候选，图工具以所有候选为 roots 做并集去重 | `calls`, `impact`, `path` |
| `BestEffortSingle` | 始终选最佳，分差 < 400 时标记 `BestEffort` 模式 | `trace`, `usages` |

**API 入口**：

```rust
pub fn resolve_symbol_input(
    store: &Store,
    input: &SymbolInput,
    policy: SymbolResolutionPolicy,
) -> Result<SymbolResolution, String>
```

**路径校验安全说明**：

`normalize_and_validate_path` 拒绝 `..` 逃逸路径和绝对路径，在计分前返回参数错误。

### 10.7 客户端介入差异

TUI / MCP+progress / MCP-no-progress 使用**完全相同的响应信封**（见 §10.4 三态终局）。
非终态只由查询层的 Focus/lazy 状态决定：handler 在当前 bounded window 内产出
`query_id`、当前可用结果和可选 `analysis.retry_after_ms`；客户端随后用
`resume_query(query_id)` 重放查询。MCP service 层不再提供第二套 waitable task、
`task_id` 或按请求超时派生的 polling contract。

Progress token 只影响观测通道，不改变终态策略：

| 模式 | Progress 通知 | 非终态判断 | 恢复入口 |
|------|---------------|------------|----------|
| TUI | 界面显示 worker activity | `analysis.retry_after_ms` / `gaps` | `resume_query` |
| MCP + progress token | 走 `notifications/progress` | `analysis.retry_after_ms` / `gaps` | `resume_query` |
| MCP 无 progress token | 无 transport notification | `analysis.retry_after_ms` / `gaps` | `resume_query` |

带 progress token 的同步请求在 handler 返回后必须先释放 request-scoped
`ToolCallContext`（关闭 progress sender），再等待 notification forwarder 排空退出；
反向顺序会让请求永久等待仍由自身持有的 sender。

## 11. Search、Context、MCP、CLI

### 11.1 Search
- FTS5 + LIKE fallback + fuzzy matching。
- `SearchQueryParser` 支持 `kind:`、`lang:`、`path:`、`name:` 前缀。
- MCP `search` 始终要求 `scope` 参数；scope 同时是搜索边界和 focus 热点。返回值必须声明该 scope 内结果是 complete 还是 partial。只有快照持有 live `JobTracker`、可由 `resume_query` 观测收敛的 refinement 才能发布 `analysis.retry_after_ms`。Search 不排队未跟踪的 focus warming；边界外覆盖统一返回终态 `closure_boundary` gap，避免后台写入阻塞后续交互式查询。

### 11.2 Context
- 基于 symbol、callers/callees、file peers、importers/dependencies 构建 Agent context (Markdown)。
- `symbol(view="detail")` 是 StoreFact 查询，只返回身份、位置、签名和可选源码；调用关系由 `symbol(view="context")` 或 `calls` 提供。detail 仅在显式 `includeCode=true` 且当前无源码事实时触发 structural focus。
- `symbol(view="context")` 支持 `includeFilePeers` 布尔参数（默认 `true`），设为 `false` 时跳过 file peers 查询，适合更快、更小的响应。
- 当符号未被索引时，`symbol(view="context")` 工具内置 lazy structural extraction（查询时按需触发完整 structural 解析）。
- **图刷新决策**：lazy structural 写新 facts 到 DB 后，`context` handler 会在调用 context builder 前执行 `force_refresh_graph()`，确保内存图快照包含刚解析的边。这关闭了 graph init 早于 handler 自身 structural extraction 的调用流缺口。

### 11.3 MCP
- 基于 `rmcp` 的 stdio JSON-RPC transport。
- **15 个工具**：MCP 工具面使用 open-first focus 机制；所有工具使用短名（无 `atlas_` 前缀）。`index` 和旧 task/wait 工具已移出 MCP。

| 组 | 工具 |
|----|------|
| Project | `project(action="open\|status\|files")` |
| Symbol | `search`, `symbol(view="detail\|context\|usages")` — 主参数 `symbol` |
| Graph / Impact | `calls(direction="incoming\|outgoing\|both", edge_kinds=[...])`, `explore`, `path`, `impact` |
| File Graph | `file_dependencies(file_path, direction="incoming\|outgoing\|both")` |
| Source Trace | `trace(kind="point\|variable\|forward\|callers")` |
| Semantic Analysis | `lifecycle`, `branch_diff` |
| Annotations / Rules | `fp_dispatches(action="add\|list\|delete")`, `domain_rules(action="add\|list\|delete\|learn")` |
| Focus state | `tasks`, `resume_query` |

- Graph 惰性初始化：首次 graph-backed tool 调用时构建 snapshot。
- Focus/lazy 写入通过 `record_lazy_writes()` 进入刷新队列；后续 graph-backed 请求由 `maybe_refresh_graph()` 批量增量刷新，累计变更过大时退化为完整 snapshot rebuild；增量刷新失败的 batch 原样回队且不重复累计 lazy-write count，避免一次性写入信号丢失。
- 后台 closure 完成时通过 `JobTracker::record_built_files` 同时写入两种视图：按 job 保留的 built-files 历史供 `resume_query` 判定与重放，以及 project-wide、去重、一次性消费的 graph-refresh 集合。`maybe_refresh_graph()` 不依赖 `replay_focus_result`，会在 `take_incremental_batch` 之前经 `FocusRuntime::take_background_refresh_files` / `QueryRuntime::record_background_built_files` drain 后者到 lazy 刷新队列。因此 fresh 请求、resume replay 和不携带 query snapshot 的 file-focused warming 都共享同一刷新边界；无需 engine 回调 listener 或 MCP 跨请求保存 closure ID。重复 drain 为空，resume 的 `materialized_files()` 与刷新队列去重保持幂等。
- 当 handler 内部触发 lazy structural 并写入新 facts（如 `symbol(view="context")` 的 Tier 3 解析），handler 显式调用 `force_refresh_graph()`（跳过缓存冷却），确保 graph 包含刚解析的边。
- `project(action="open")` 不索引，只同步激活项目并打开持久化的 `project/.atlas/atlas.db`；MCP 不暴露 storage mode。
- MCP 查询路径不探测或同步整个工作树。磁盘文件与持久化索引的全项目同步由显式 CLI `atlas sync`/`atlas index` 负责；查询触发的 lazy extraction 只更新当前 scope/closure，并通过 `tasks`、`query_id` 和 analysis envelope 暴露状态。
- 显式全项目索引只通过 CLI `atlas index` 执行。
- `search` 的 `scope` 永远强制参数；scope 同时是结果边界和 focus seed，即使存在 manual full index 也不省略。
- MCP 不支持 `background=true`；未完成的 scoped focus/lazy 工作通过 `analysis.retry_after_ms` + `query_id` 表达，终态限制通过 `gaps` 表达，客户端使用 `resume_query` 重放。
- 结果截断 25KB，额外 content block 标注截断信息。

### 11.4 CLI
核心命令：`status`, `doctor`, `index`, `sync`, `files`, `mcp`。

裸 `atlas` 启动交互式 TUI，使用当前目录的 `.atlas/atlas.db`。如果没有可用 DB
或 schema 初始化失败，`atlas` 会先保留不可用 DB 为 `.corrupt.<timestamp>` 备份，
创建新 schema，并运行与 `atlas index` 默认值一致的 structural index；
索引完成后才启动 TUI。已有基础 index 或更高等级 index 时直接进入 TUI。
TUI 状态栏必须显示当前
index mode（empty/manifest/structural/full/partial）。

TUI 首跑索引属于 CLI 入口前置步骤，不属于 TUI 内交互状态机：

- “基础 index”定义为所有已索引文件至少有 fresh `manifest:complete` extraction state；
  `structural:complete` 和 `dataflow:complete` 均满足直接进入 TUI 的条件。
- `empty`、`partial`、无法打开 DB、schema init 失败都不能直接进入 TUI；必须先恢复/创建 DB
  并完成默认 structural index。
- 入口层只能调用共享 `IndexPipeline` 完成默认 structural index；不得在 TUI 模块内复制 discovery、
  hash check、cleanup、extraction、resolution 或 graph build phase。
- 损坏 DB 的主文件、WAL、SHM 文件应尽量一起保留为 `.corrupt.<timestamp>`，再重新创建
  `.atlas/atlas.db`。
- TUI 应只消费已存在的 Store/Graph/Search 能力，并在状态栏展示 index mode；lazy structural
  仍可在搜索无结果时按既有规则触发，并在完成后刷新状态栏 mode。

## 12. Analysis / Trace

Atlas 不包含污点分析（taint analysis）。产品主线为变量来源追踪与调用路径查询：

```text
用户指定位置 / callsite / 问题变量
  → 定位 DataNode / BindingUse / ReferenceUse
  → backward slice 追踪变量来源
  → 结合 callers/callees 找到可能调用路径
  → 输出 bounded evidence 给 Agent 分析
```

### 12.1 Trace 查询入口

- `trace(kind="point")` — 解析源码位置到 full context。
- `trace(kind="variable")` — backward dataflow walk 获取变量来源。
- `trace(kind="callers")` — backward call edge walk 获取调用者链路（单链）。
- `trace(kind="forward")` — forward call edge walk 回答"how does A reach B"。

### 12.2 输出契约

所有 trace 工具返回 `TraceQueryResponse<T>` envelope：
- `ok`, `kind`, `capability`, `partial_result`, `diagnostics`, `result`。
- 详见 [`trace-contract.md`](./trace-contract.md)。
- 注：`partial_result` 是 trace 工具特有字段（frozen 合约），与外层 MCP 响应信封的三态终局模型（`retry_after_ms` + `gaps`）独立。非 trace 工具的 `partial_result` 已不再使用。

Trace/MCP lazy contract：
- MCP trace 入口必须优先通过 high-level `Engine`，由 engine 触发必要的 lazy dataflow；raw analysis consumer 不负责触发 lazy。
- 只要 lazy structural、lazy dataflow 或 focus refinement 被触发，响应就必须通过统一 public analysis view 暴露 `analysis`（含 `retry_after_ms`）和 `gaps`。即使 trace 没有找到 path，也必须说明当前结果可用性、已知缺口和下一步动作；默认契约不暴露 `lazy_diagnostics`。
- CFG-consuming tools（如 `branch_diff`、`lifecycle`）如果已经 re-query 到 CFG 并基于 CFG 产出结果，`analysis.basis` 必须包含 `cfg` 或 `dataflow`；不能同时返回 CFG 分析结果又不声明已使用该能力。
- 能力相关结论（safe/unsafe conclusions）通过 `analysis.basis` 使用的数据源和 `gaps[].reason` 缺失原因直接表达，不再使用独立的 `analysis_contract` 结构体。

### 12.3 Lifecycle、Branch Diff 与 Semantic Impact

Atlas 的 lifecycle/branch 分析是 analysis 层能力，直接消费 `cfg_nodes`、`cfg_edges`、`data_nodes`、`dataflow_edges` 和 domain-rule consumer，不建立独立 Function IR。

#### 12.3.1 核心架构

```
CFG Builder + DataFlow Builder (extraction)
           │
           v
     EffectComposer (&dyn OwnershipContract)
           │
           v
     CfgNode.semantic_effects: Vec<SemanticEffect>
           │
    ┌──────┴──────┐
    v             v
branch_diff    lifecycle
(semantic)     (semantic)
```

**SemanticEffect**（`types::effects`）：语言无关的多效应表示，每个 CfgNode 可携带多个效应。`OwnershipContract` trait 定义 `classify_return` / `classify_consumption`，由各语言实现。

**EffectComposer**（`analysis::effect_composer`）：消费 CFG + DataFlow + `&dyn OwnershipContract`，通过 range-overlap 匹配 + DFS 反向追踪 DataFlow 边，将单语句分解为多条 `SemanticEffect`（Alloc/Free/Store/Nullify 等），并构建函数级 `TransferGraph`（field→value 映射）。

持久化 CFG 保存控制流事实，不要求把查询相关的 `semantic_effects` 预写回数据库。
`lifecycle`、semantic impact 和 `branch_diff` 在查询时加载目标函数的 CFG/dataflow，执行
`EffectComposer`，并把组合结果附着到内存中的 CFG 副本。这样 structural/CFG 缓存保持
语言事实层，ownership/domain rules 的变化可以立即影响分析而无需重建索引。

**MCP 入口**：工具侧经 `AnalysisRuntime::run_lifecycle` / `run_branch_diff`（或 impact 的
`semantic_composition_for_function` + `analyze_*` helper）完成 ensure → compose → engine；
analysis crate 内的 `FieldLifecycleEngine` / `BranchDiffEngine` 仍是引擎实现，但 MCP handler
不得直调。

**branch_diff_semantic**：基于 `EffectComposition` 比较分支路径的语义效应差异，输出结构化 `BranchDiffIssue`（含 asymmetry kind、confidence、evidence）。MCP `branch_diff` tool 默认使用 semantic 路径（`semantic=true`）。

**lifecycle**：`transfer_state` 读取查询时组合的 `semantic_effects`（多效应按序处理），
legacy 路径已移除。跟踪目标既可以是 canonical field path，也可以是函数局部资源变量；
local 必须精确匹配，field 使用 canonical field matching。`FieldTransition` 按效应记录，
DoubleFree/UseAfterFree 检测基于每次状态转换。C/C++ 默认资源语义包括 libc alloc/free，
以及 Linux 常见的 `kmalloc`/`kzalloc`/`kcalloc`/`kvcalloc`/`vmalloc` 与
`kfree`/`kvfree`/`vfree`。能力门控：MCP lifecycle 仅接受 C/C++ 符号（非 C/C++ →
`unsupported_language` gap，非 error 终态）。

#### 12.3.2 多语言支持

`OwnershipContract` trait + `ConsumptionStyle` 区分 5 种消费模式（ExplicitCall/MethodCall/Implicit/Deferred/ContextManaged），`ResourceOpConfig::default_for(Language)` 通过 `producers`/`consumers` CalleeMatcher 覆盖 11+ 语言。每种语言混合使用多种消费风格（如 C 同时使用 `free()` 和 impliclit scope exit）。

#### 12.3.3 当前约束

- CFG effect annotation、field lifecycle 和 branch diff 先以 C/C++ 为主要适用语言。
- `FieldLifecycleEngine` 对字段或局部资源状态做路径敏感分析，状态包括 `Unknown`、`MaybeLive`、`Assigned`、`Freed`、`Nullified`、`Escaped`、`Returned`、`Invalidated`。
- `BranchDiffEngine` 比较 sibling branch 的语义效应差异。
- `LifecycleProof` 在 domain rules 覆盖相关 free/alloc/owned pattern 后，将 pattern observation 升级为 rule-backed proof。
- `impact` 可在 semantic 模式中组合 graph impact、domain rules 和 lifecycle 分析，输出 semantic impact 摘要。

禁止事项：
- 不建完整跨函数 dataflow 全量分析来支撑 C/C++ lifecycle。
- 不建独立 Function IR；如需要新增表达能力，优先扩展 CFG/dataflow facts 或 analysis 输出。
- 不让 `domain_rules` 核心解释 C/C++ 语义。

## 13. Cargo Features

| 层级 | Features |
|------|----------|
| 默认 | `typescript`, `javascript`, `python`, `java`, `c`, `cpp`, `arkts`, `go`, `csharp`, `rust`, `php`, `ruby`, `kotlin`, `cangjie` |
| MCP | `mcp` (independent of language features) |

## 14. 引擎拆分与 Corpus 边界

- `atlas-engine` 的 supported 顶层 facade 可作为独立 crate 使用；稳定范围是高层入口及其公开签名类型闭包。为 MCP/CLI 暂留的 workspace-internal 模块不属于该承诺。
- `atlas-engine` 不依赖 CLI 参数解析、MCP transport 或交互格式。
- Corpus（大型多版本源码索引系统）不并入 Atlas 主体。
- Corpus 以 Git blob/tag/path/version mapping 为核心索引模型，不复用 Atlas 的 path-based `FileId`。

## 15. 相关文档

- 产品需求：[`requirements.md`](./requirements.md)
- 路线图：[`roadmap.md`](./roadmap.md)
- 测试规范：[`testing.md`](./testing.md)
- Trace 契约：[`trace-contract.md`](./trace-contract.md)
- Domain Rules 语言扩展指南：[`domain-rules-language-guide.md`](./domain-rules-language-guide.md)
- 性能基线：[`performance.md`](./performance.md)

## 16. 已知限制

### Lazy Indexing

- **构建期间的并发读取**：当请求遇到处于 `AlreadyBuilding` 状态的 extraction job 时，它立即返回而不等待构建完成。MCP 响应通过 `analysis.retry_after_ms` 和 `query_id` 表达可恢复 pending；`resume_query` 重新观察 fresh `extraction_state` 和 facts，完成或失败后收敛为终态结果。

- **Include root auto-detection**: `project_root/include/` is auto-detected.
  Additional directories can be passed per-request via the `include_roots`
  MCP parameter.   Large C/C++ projects (e.g., the Linux kernel) may need project-specific
  include directories like `arch/<arch>/include/` or `include/generated/`.
  See MCP README for usage.

- **冷项目首查成本**：`project(action="open")` 只激活项目，不扫描全树。首次 scoped search/trace 由 Focus bootstrap 建立 file inventory / symbol hints 并做有限闭包提取，因此可能先返回非终态响应；客户端按 `query_id` 和 `analysis.retry_after_ms` 调用 `resume_query`。若需要稳定的项目级完整缓存，应在 MCP 外运行 `atlas index`。

### Graph

- **Graph refresh after lazy extraction**: Production code uses
  `replace_files_in_place` (via `refresh_graph_for_files`) —
  old nodes/edges for changed files are removed, then fresh data
  is loaded from the store and merged.  For large change sets
  (> 500 files), falls back to full `GraphEngine::from_store()`
  rebuild.  `merge_delta_in_place` is an append-only helper
  used internally by `replace_files_in_place` for the merge step.

### Linux 增强（post-extract hook）

- **挂载点**：`extraction::apply_post_extract_hooks`，由 `extract_file_with_mode` 在所有成功返回路径调用（Manifest / ResolutionSymbols / Structural / Full / LazyDataflow）。
- **ResolutionSymbols 层**：仅 `EXPORT_SYMBOL` 标志被持久化。`initcall`/`module_init` 边和 `SYSCALL_DEFINE` diagnostics 仅持久化到完整的 `structural` 层（该层写入 `raw_edges`）。
- **展示**：语言能力对用户只暴露 `CapabilityLevel` + `confidence_floor`；`FeatureMatrix` 细节用于内部门控，不在 doctor/status 默认输出中展开。
