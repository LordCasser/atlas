# Atlas 架构文档

本文是 Atlas 的**单一权威架构文档**，合并了架构约束、当前实现状态、已落地的设计决策（Lazy Index、Lazy UX、DataflowFull 摘要层、Domain Rules 通用化）。本文对代码实现有约束力；当代码与本文冲突时，以本文为准并同步修正代码。

## 1. 总体原则

1. Atlas 是 CodeGraph-inspired，不是 CodeGraph-compatible。
2. Rust 实现使用 trait、newtype ID、enum、immutable facts、batch write、read snapshot 和 Rayon。
3. SQLite 是持久化源（`.atlas/atlas.db`）；内存图只作为查询加速和分析工作集。
4. MCP 是一等入口；CLI、MCP、context 输出都必须可限制大小。
5. 所有启发式语义结果必须可解释，不能把低置信度结果伪装成精确结果。
6. 分析等级相关改动必须验证完整入口矩阵：CLI 自有管线、shared filesync pipeline、sync、lazy structural、lazy dataflow、高层 Engine 和 raw analysis consumer。任何模式或 capability/status/precision 变化都不能只验证单一路径。

## 2. 模块边界与依赖方向

### 2.1 Crate 结构

项目是 16 个 Cargo package 的 workspace：

```text
crates/
  atlas-engine/        facade crate，re-export types/db/extraction/resolution/graph/analysis/search/context/filesync/lazy, dossier
    crates/types/      ID、enum、IR、binding、dataflow、CFG、trace 查询类型、capability profiles
    crates/workspace/  ProjectRoot、WorkspacePaths、SourcePath
    crates/db/         SQLite schema v1、Store API、readers、schema 迁移基础设施
    crates/extraction/ tree-sitter 解析、query、scope、semantic binder、lexical binder、dataflow、CFG、worker pool
    crates/resolution/ builtin filter、scope/container/import/include/name matching、PathAliasResolver
    crates/graph/      GraphBuilder、GraphSnapshot、GraphEngine
    crates/analysis/   变量来源追踪与调用路径查询、SummaryBuilder、CrossFunctionBridge
    crates/domain_rules/ 语言无关 domain rule store/match/learning 核心
    crates/search/     FTS5、LIKE/fuzzy、query parser、scoring
    crates/context/    Agent context builder (Markdown)
    crates/filesync/   file discovery、change detection、file lock、watcher
    crates/lazy/       Lazy dataflow engine — on-demand analysis with budget caps
    crates/dossier/    Symbol Dossier builder
  atlas-mcp/           MCP server (rmcp stdio JSON-RPC)、15 open-first focus tools
  atlas-cli/           CLI binary + commands + integration tests
```

### 2.2 依赖方向（严格无环）

```text
atlas-cli → atlas-engine, atlas-mcp
atlas-mcp → atlas-engine
atlas-engine → types, workspace, db, extraction, resolution, graph, analysis, search, context, filesync, lazy, dossier, domain-rules
filesync → graph, resolution, extraction, analysis, db, types, workspace
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
| `db` | schema、读写、迁移 | 不承载语言语义规则 |
| `extraction` | 单文件 tree-sitter facts 抽取 | 不做跨文件 resolution |
| `resolution` | 更新 resolved facts | 不直接承担展示格式 |
| `graph` | 从 resolved facts 构建 symbol graph | 不混入 dataflow/CFG |
| `analysis` | 消费 dataflow、CFG 和 call graph；trace/slicing | 不破坏底层 facts |
| `domain_rules` | 语言无关 rule 存储、匹配、学习候选、registry 校验 | 不解释 C/C++ ownership、Rust safety 等语言语义 |
| `lazy` | 按需 dataflow 加载，budget-capped | 不改变 extraction 语义 |
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
- ArkTS 复用 TypeScript grammar，但 language 必须是 `arkts`。
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

### 6.1 Schema（当前版本：V1）

当前 schema 版本为 V1。软件处于快速原型期，新库以主 DDL 为准，不保留
旧 schema 运行时补丁路径。schema contract 改变时直接更新主
DDL、调用方和文档，并要求重新建库/重索引；不得在 `Store::init_schema`
中累积旧版本补丁路径。

主要表（23 张）：

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

### 7.1 Lazy Dataflow

analysis 层按需加载 dataflow facts（而非全量预加载），通过 `LazyWindow` 控制分析范围。结构性 lazy 提取 budget-capped (18s/30 files)；后台 preparse 使用更宽预算 (60s/100 files)。`ExtractionMode::LazyDataflow` 支持增量按需抽取。

等级路径约束：
- `Manifest`、`ResolutionSymbols`、`Structural`、`LazyDataflow`、`Full` 是 extraction mode；`DataflowBasic/DataflowFull` 是语言 capability；`PrecisionTier` 是 lazy 结果质量。三者含义不同，禁止混用。
- `atlas index` CLI、`filesync::run_index_pipeline`、`atlas sync`、`LazyStructuralService`、`LazyDataflowService` 和 `analysis::TraceEngine` 是不同入口。修改任一等级行为时，必须确认这些入口是否受影响。
- 高层 `Engine::trace_variable` 负责触发 lazy dataflow；raw `analysis::TraceEngine` 只消费已存在 facts。用户入口应优先走高层 Engine，除非明确只需要底层已持久化数据。
- `Full` 必须在 facts、summary、extraction_state、capability mask 和用户可见 status 上都表现为完整分析；不能只在 facts 表中写入 dataflow/CFG。
- lazy 路径必须复用或严格对齐 structural facts，尤其是 callsite、symbol、scope、content_hash 和 capability mask；不得重建一套会与 structural DB 状态漂移的事实解释。

分析等级的长期语义如下，所有入口必须与此表保持一致：

| 等级 | 写入事实 | 继续阶段 | 用户可见要求 |
|------|----------|----------|--------------|
| `Manifest` | 仅顶层 manifest symbols | 不做 references、resolution、graph、summaries | 不能暴露或暗示 structural/dataflow 已完整 |
| `ResolutionSymbols` | symbols、imports、scopes、scope tree | 不做 references、dataflow、callsites | 仅作为 dependency/lazy resolution 目标层 |
| `Structural` | symbols、references、imports、scopes、callsites、exports、call edges | resolution + graph build | 能回答结构性搜索、context、caller/callee；不能宣称 dataflow/CFG |
| `LazyDataflow` | window 内 unit dataflow、binding uses、可用时 CFG | 不重写 structural facts | 必须记录 unit extraction state、pending/partial 诊断和 capability mask |
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

### 7.2 跨函数桥接（DataflowFull）

Schema V1 实现了持久化摘要层：

```
dataflow_edges    = intra-procedural, fine-grained, direct edges (不变)
function_summary  = intra-procedural, transitive-closure, per-function (持久化摘要层新增)
trace             = inter-procedural, by composing summaries along call graph
```

- `SummaryBuilder` 从 dataflow_edges BFS 计算函数摘要。
- `SummaryStore` 持久化 4 张摘要表，支持全量构建（`build_all`）和增量构建（`build_for_function`）。
- `CrossFunctionBridge` 替代了旧的 runtime `SummaryEdgeProvider`，实现 ArgToParam 和 ReturnToCall 桥接。
- 旧 DB 无摘要表时不做 runtime BFS fallback；要求按当前 schema 重新建库
  或重索引。
- 增量失效：sync 时删除受影响函数的摘要行并重建。

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
  capability_level       → None / Symbolic / DataflowBasic / DataflowFull
  supported_features     → 人类可读的 feature 名称列表
  unsupported_features
  known_limitations
  confidence_floor       → 0.0-1.0
  features               → FeatureMatrix（类型安全的逐 feature 查询）
```

### 9.2 权威能力表

从代码 `capability.rs` 导出，与实现保持同步：

| Language | Level | CFG | Confidence | Interprocedural | Note |
|----------|-------|:---:|:---:|:---:|------|
| TypeScript | DataflowFull | ✓ | 0.60 | ✓ (ArgToParam + ReturnToCall) | Summary tables + CFG |
| JavaScript | DataflowFull | ✓ | 0.60 | ✓ (ArgToParam + ReturnToCall) | 共享 TS adapter |
| Python | DataflowFull | ✓ | 0.72 | ✓ (ArgToParam + ReturnToCall) | scope-chain-aware binding |
| Java | DataflowFull | ✓ | 0.75 | ✓ (ArgToParam + ReturnToCall) | |
| C | DataflowFull | ✓ | 0.73 | ✓ (ArgToParam + ReturnToCall) | 函数指针 limited depth 3 |
| C++ | DataflowFull | ✓ | 0.70 | ✓ (ArgToParam + ReturnToCall) | 模板/重载/ADL 不建模 |
| ArkTS | DataflowFull | ✗ | 0.60 | ✓ (via summary tables) | TS grammar fallback |
| Go | DataflowFull | ✓ | 0.78 | ✓ (ArgToParam + ReturnToCall) | 泛型未捕获 |
| C# | DataflowFull | ✗ | 0.72 | ✓ (via summary tables) | partial classes 未合并 |
| Rust | DataflowFull | ✓ | 0.70 | ✓ (ArgToParam only; ReturnToCall gap) | 宏/burrow 不建模 |
| PHP | DataflowFull | ✗ | 0.62 | ✓ (via summary tables) | 参数 DataNode 抽取 gap |
| Ruby | DataflowFull | ✗ | 0.65 | ✓ (ArgToParam + ReturnToCall) | block/yield gap |
| Kotlin | DataflowFull | ✗ | 0.67 | ✓ (via summary tables) | extension receiver `this` binding |
| Cangjie | DataflowFull | ✗ | 0.65 | ✓ (ArgToParam verified; ReturnToCall basic) | postfixExpression callSuffix |

约束：
- capability profile 属于 engine/analysis 边界；CLI/MCP/context 只能读取并展示。
- 每个查询结果必须携带实际使用的语言能力信息。
- 查询请求超出当前语言边界时，返回 partial result + diagnostics，不返回空数组。
- 低置信度 fallback 必须带 `confidence`、`strategy` 和 `provenance`。

### 9.3 FeatureMatrix 能力门控

- `trace_variable`：门控 `local_dataflow.is_supported()`。
- `trace_callers`：门控 `call_graph.is_supported()`。
- `trace_point`：始终可用。
- `derive_capability_level()` 的升级条件：
  ```
  DataflowFull = local_dataflow + use_def + interprocedural_summaries
                 + returns_flow + call_arguments (all supported)
  DataflowBasic = local_dataflow + use_def (supported)
  Symbolic      = symbols + references (supported)
  ```

## 10. Extraction 实现

当前抽取层：
- `ParseWorkerPool` — 支持 max file size、panic isolation、结构化 `ExtractionError` 和 `IndexReport`。
- `SemanticBinder` — 统一填充 source/scope/binding。
- `LexicalBinder` + `DataFlowBuilder` — 词法绑定与数据流。
- `CfgBuilder` — 函数级 CFG（TS/JS/Python/Java/C/C++/Go/Rust）。
- Golden test framework 覆盖 14 种语言。

已知限制：
- CFG 不覆盖 try/catch/finally、switch/case、async/await、labeled break/continue（所有语言）。
- Java/C/C++/ArkTS/Go/C#/Rust/PHP/Ruby/Kotlin/Cangjie 的 CFG 未实现或部分实现（见能力表）。
- per-file timeout 尚未完全强制。

### 10.1 查询时 lazy index 架构

当前系统实现了 manifest、resolution_symbols、structural、dataflow 多层 lazy index 结构，并通过 extraction state、job tracking、analysis contract、query resume 和 investigation state 提供可观测的查询时渐进分析体验。

#### 10.1.1 Layer 层次结构

提取精度按层（layer）建模，从最轻量到最完整：

| Layer | 说明 |
|-------|------|
| `manifest` | 仅顶层符号（type/function/class 声明），无引用、无 scope。通过 `--analysis manifest` 产生。 |
| `resolution_symbols` | 最小符号层，仅供跨文件引用解析使用。包含 symbols、imports、scopes，不包含 references、callsites、dataflow、raw_edges。 |
| `structural` | 完整符号、引用、scope、边。通过 `--analysis structural` 或 lazy structural 产生。 |
| `dataflow` | 所有 structural 事实 + per-function dataflow/CFG。通过 `--analysis full` 或 lazy dataflow 产生。 |

Layer 通过 `SymbolDef.layer` 和 `extraction_state.layer` 字段标识。

#### 10.1.2 Extraction job 生命周期

所有 lazy extraction 触发均通过 `extraction_jobs` 表追踪，确保可观测性和并发去重：

```
queued → building → complete
                  → failed
```

- **queued**: 作业已注册但尚未开始。
- **building**: 正在执行提取。
- **complete**: 提取成功完成。
- **failed**: 提取失败（`error_msg` 记录原因）。

Job ID 基于时间戳生成（`extract_{microsecond_hex}`）。同一 `(file_id, unit_id, layer)` 在 `queued`/`building` 状态下有且仅有一条活跃记录；文件级 job 的 `unit_id` 为 `NULL`。并发请求通过 claim API 的 dedup 语义使用同一 job_id。

Job tracking 表结构：参见 `db::schema::SCHEMA_DDL` 中的 `extraction_jobs` 表。

#### 10.1.3 精度等级

查询响应携带精度信息，告知消费方结果的完整度：

| Precision Level | 条件 |
|-----------------|------|
| `Exact` | 目标文件有完整 structural+dataflow，预算未超。 |
| `PartialExact` | structural 完整但 dataflow 被预算截断。 |
| `DegradedStructural` | structural 预算超支，仅有 manifest 或 resolution_symbols。 |
| `LocalDataflowOnly` | dataflow 仅对当前函数可用，无跨文件传播。 |
| `ManifestOnly` | 仅顶层符号可用。 |
| `Unavailable` | 文件未索引或语言不支持。 |

#### 10.1.4 In-flight 一致性

- **去重**: `extraction_jobs` 表确保同一 file+unit+layer 不会并行构建两次。
- **读写一致性**: 每个 handler 在触发 lazy extraction 后，在自己的写事务中可见刚写的数据；读操作通过 `StoreReader`（独立只读连接）访问。
- **Delta graph refresh**: lazy structural 写入后，通过 incremental refresh 或必要时完整 snapshot rebuild，确保图查询能看到新边。

#### 10.1.5 Closure 与 Linux 增强边界

- `ClosurePlanner`: 基于 import/include 图计算依赖闭包，确保被引用文件的 `resolution_symbols` 层先于主文件的 structural 层构建。
- `resolution_symbols` 层实现: 轻量提取模式，产出 symbols + imports + scopes（无 references/callsites/dataflow/raw_edges），供跨文件引用解析使用。
- Linux 增强边界: 对 C 语言的特定惯用法（syscall 宏、EXPORT_SYMBOL、initcall、static inline）在提取后进行后处理增强，不改动通用提取管道。

#### 10.1.6 已实现的基础阶段

**P0: Scope Index** — 允许 `--include`/`--scope`/`--exclude` 限制索引范围，降低大型项目 index 时间和 DB 体积。

**P1: Manifest Extraction** — `ExtractionMode::Manifest`：仅提取顶层符号，为 lazy structural 提供候选源。通过 `symbols.layer` 字段区分。

**P2: Lazy Structural** — 查询时按需触发完整 structural extraction。`LazyStructuralService` + `CandidateProvider` + `StructuralLoader`。

**Lazy UX** — `CapabilityMask`、`AnalysisContract`、`QuerySnapshot`、`resume_query`、`tasks` 和 session-scoped `Investigation` 已接入 MCP 查询路径。

#### 10.1.7 Lazy 状态与任务边界

文件级状态、单元级 dataflow 状态和进行中任务已经统一为
`extraction_state` / `extraction_jobs` 两个表：

- **完成状态**：文件级状态以 `extraction_state.unit_id IS NULL` 为准，必须与 `files.content_hash` 匹配；单元级 dataflow cache 以 `extraction_state.unit_id IS NOT NULL` 为准。
- **进行中状态**：所有 on-demand structural、resolution_symbols 和 dataflow 构建都必须 claim extraction job。dataflow 使用 unit-scoped job key，避免同一函数/顶层单元被前台 trace 和后台 prewarm 重复构建。
- **MCP 可观测性**：`status` 只根据 fresh layer 分布推导 `manifest`、`partial_structural`、`structural`、`structural+lazy`、`full`，不能把 manifest-only 误报为 structural。Agent 判断是否可用、是否等待、是否重试只能读取 public `analysis` 和 `work`，不能读取 raw extraction job 或 pending/partial 旧字段。
- **Search 执行模型**：MCP search 先做 store-backed manifest 查询，再对候选文件做定向 lazy structural；只有候选为空且 scope 很小时才同步解析整个 scope。大 scope 不做同步全量 structural，避免把一次搜索变成隐式全项目索引。

#### 10.1.8 CancellationToken 可中断提取

Lazy extraction 的 budget 约束已从"循环守卫"升级为"可中断提取"：

- **`CancelCheck` trait**（`extraction/cancel.rs`）：`fn is_cancelled(&self) -> bool`。`NeverCancel` 作为不需要取消能力的 no-op sentinel。
- **`extract_file_with_mode_cancellable`**（`extraction/extract.rs`）：在以下检查点插入 token 检查（CP1-CP4）：
  - CP1: `tl_parse()` 之前 — 进入时预算已耗尽则跳过 parse
  - CP2: 符号查询之后
  - CP3: 引用查询之后
  - CP4: 导入/作用域查询之后
- **`collect_captures`**（`extraction/query_helpers.rs`）：流式捕获循环中每 ~100 次迭代检查取消
- **`ReindexOutcome`**（`lazy_structural.rs`）：`Built` / `Cancelled` 枚举，在 DB 写入前（CP5）和 `extract_file` 调用前（CP6）检查。`Cancelled` 设置 `budget_exceeded=true` 但不计入 `files_built`
- **`LazyBudget`** 实现 `CancelCheck`（`is_cancelled = cancelled || time_exceeded`）。`can_continue()` 超时时自动调用 `cancel()`
- `extract_file_with_mode` 保留原签名，作为 `_cancellable` + `&NeverCancel` 的包装器

此设计不修改 tree-sitter C FFI，不引入 signal，不增加线程。Cancellation 是正常降级路径，产生 precision 降级而非 MCP tool error。

#### 10.1.9 Analysis Contract、Query Resume 与 Investigation

所有触发 lazy extraction 的 MCP 响应必须暴露 `analysis_contract`，明确当前数据能支持和不能支持的结论：

```text
analysis_contract
  safe_conclusions       当前 capability mask 支持的结论
  unsafe_conclusions     缺失能力导致不能证明的结论
  capability_summary     mask bits、best capability、文件能力分布
  refinement_available   当前结果是否可通过后台构建继续提升
```

`analysis_contract` 只声明当前结果能证明什么、不能证明什么，以及是否存在
可提升空间；它不承载后台任务列表。MCP 不再暴露可等待 task API：`tasks`
只观测当前 session 的 focus/lazy 活动，查询恢复通过 `resume_query` 重放
query snapshot。

`query_id` 是 MCP 层概念，不复用 extraction job id。查询快照保存在 MCP session 内存中，默认 TTL 5 分钟；`resume_query(query_id)` 使用原 tool 参数和 `LazyWindow` 重新执行查询，返回完整增强结果而不是 diff。MCP server 重启后 query snapshot 丢失。

`Investigation` 是 MCP session 级隐式调查上下文，不提供用户可见的 create/close API。分析类工具会根据 symbol、position 或 field focus 更新 active investigation，并把相关文件/符号和期望能力传给 lazy 调度器。TTL 同样为 5 分钟。

#### 10.1.10 统一对外分析认知界面

MCP 分析响应必须提供一套稳定的 public analysis view，作为 Agent 判断
“当前结果是否可用、覆盖到哪里、还缺什么、是否需要等待”的唯一认知界面。
内部可以有多套状态机，但外部不能要求 Agent 理解 lazy/focus/bootstrap/
resolver/graph/corpus 的内部生命周期。

```text
analysis
  state       ready | usable_partial | building | blocked | failed | stale
  scope       repo | local | file | symbol | query | corpus
  summary     当前可用事实和限制的短说明
  next_action use_result | use_result_or_wait_for_refinement | wait |
              narrow_scope | run_full_index | retry | inspect_gaps
```

外部响应只允许通过以下字段表达分析状态：

| Public field | 职责 |
|--------------|------|
| `analysis` | 当前结果的可用性、范围和下一步动作。 |
| `precision` | 覆盖范围和语义置信度。 |
| `coverage_counts` | 公开 coverage label 的数量分布。 |
| `gaps` | 明确、可解释的已知缺口。 |
| `analysis_contract` | 当前数据能支持/不能支持的结论。 |
| `work` | 后台提升、等待和重试状态。 |

内部状态源包括 `extraction_state`、`extraction_jobs`、query snapshot、
lazy diagnostics、focus closure coverage、bootstrap tiers、scoped resolution、
graph refresh、TaskManager、以及未来 atlas-corpus 的 repo/version/branch
状态。它们必须先归一化成 public analysis view，不得直接泄漏为新的
public 字段或要求 Agent 组合多个内部状态自行判断。

状态归一规则：

| Internal condition | Public interpretation |
|--------------------|-----------------------|
| full-index facts fresh and query evidence complete | `analysis.state=ready`, `precision.coverage=repo_complete` |
| closure/local scope complete, repo boundary未展开 | `analysis.state=ready` 或 `usable_partial`, `precision.coverage=local_complete`/`boundary` |
| manifest/basic facts only but result仍可用于名称定位 | `analysis.state=usable_partial`, `precision.coverage=basic` |
| background work can improve result | `analysis.next_action=use_result_or_wait_for_refinement`, `work.status=running|queued` |
| current query cannot be answered without more work | `analysis.state=building`, `analysis.next_action=wait` |
| budget、fanout、语言能力或配置限制阻止证明 | `analysis.state=blocked`, `analysis.next_action=inspect_gaps|narrow_scope` |
| source hash or layer freshness mismatch | `analysis.state=stale`, `analysis.next_action=retry` |

`lazy_diagnostics`、`focus`、closure id、bootstrap tier、scheduler priority、
raw extraction job id、corpus version worker id 等只能作为内部诊断或 debug
信息存在；默认 MCP 契约不得暴露它们。旧 public 字段必须删除，不做双写、
别名或过渡输出。

#### 10.1.11 Focus Runtime 与 Lazy 的关系

Focus 是查询时 lazy 机制的下一代控制平面，不是新的 extraction
pipeline。Lazy 负责按需构建 facts；Focus 负责围绕用户意图决定构建哪些
facts、按什么顺序构建、在哪个 closure scope 中可见，以及如何声明
analysis/precision/gaps/work。

长期边界：

- `LazyStructuralService`、`LazyDataflowService`、`ExtractionMode`、
  `extraction_state` 和 `extraction_jobs` 保留为事实构建、缓存、freshness、
  in-flight dedup 和可观测性边界。
- `FocusRuntime` 是 MCP 查询时唯一控制入口。MCP handler 只生成
  `QueryIntent`，不得直接组合 lazy structural/dataflow、resolver 或 graph
  builder。
- `LazyOrchestrator`、`LazyCoordinator` 和 MCP `ensure_structural_for_*`
  属于旧控制平面。它们的 job claim、budget、query_id、prewarm 和
  diagnostics 语义迁入 `FocusRuntime` / `ClosureEngine` /
  `BootstrapManager` 后，应删除或内化。
- Focus resolution 写 closure-scoped `reference_resolutions` 和 scoped graph
  overlay；只有 full-index/shared pipeline 可以更新全局
  `references.resolved_*` 和 repo-wide `symbol_edges`。
- `Precision { coverage, confidence }` 是 Focus 结果的主语义；
  `PrecisionTier` 不进入 public MCP contract。仍存在的内部使用点必须改为
  `Precision` 或局部私有 adapter，不能作为响应字段保留。
- Focus 是内部机制，不是 public response surface。默认 MCP 响应和
  `atlas_status` 不暴露 `focus`、closure id、scheduler priority、
  bootstrap tier 或 focus-specific pending queue；只暴露公开语义的
  `precision`、`coverage_counts`、`gaps` 和统一 `work`。

### 内容哈希一致性

当 `upsert_resolution_symbols` 检测到磁盘上的文件内容自上次 `files` 行写入以来已经变更（内容哈希不同），它会在同一事务中原子性地更新 `files.content_hash`。所有之前存在的更丰富层（structural、dataflow）变为过期状态，因为它们记录的 layer hash 不再匹配更新后的 file hash。在下次 lazy 访问时它们将从当前内容重建。

此"安全更新"策略保证渐进式富化永不会悄悄提供过期数据，代价是可能需要重建过期的层。

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

### 10.4 长操作进度与取消

跨 CLI/TUI/MCP 的长操作（index、sync、focus extraction、trace）使用统一
取消和降级原则：

- `ProgressSink` trait：入口注入终端进度、MCP notification 或 no-op。
- `CancelToken`：前台/后台均可中断执行，取消是正常降级路径。
- CLI/TUI 的显式 `index` / `sync` 可以是长操作；MCP 不暴露 `index`，也不
  暴露 waitable task API。
- MCP 的 `project(action="open")` 只同步激活项目，不扫描全树、不索引。
- MCP scoped 查询触发 focus/lazy materialization；响应通过
  `analysis_contract`、coverage/precision 字段和 diagnostics 表达当前结果
  是否完整可用。
- `tasks` 仅用于观测当前 session 的 focus/lazy 活动；`resume_query`
  通过 `query_id` 重放最近查询，不能等待任意后台 task。

普通分析响应只解释本次结果；全局项目状态通过 `project(status)` 查询。

分析类 tool response 只有在后台工作和本次结果直接相关时才携带 `work`：

- 本次查询触发了 lazy extraction、focus refinement、graph refresh 或 corpus
  局部构建。
- 当前结果是 `usable_partial` / `building` / `stale`，且后台工作会改变本次
  查询的质量或可用性。
- `analysis.next_action` 是 `wait` 或 `use_result_or_wait_for_refinement`。
- 响应需要给出本次查询可等待的 public `task_id`。

不得把无关的全局 indexing、project activation、corpus sync、预热队列深度
塞进每个 tool response。无关后台状态属于 status/task 工具，不属于普通查
询结果 envelope。

```text
analysis response
  analysis          required for analysis tools
  precision         required when semantic confidence matters
  coverage_counts   optional; include when coverage distribution explains result quality
  gaps              optional; include when non-empty or state is partial/blocked
  analysis_contract required for tools that make semantic claims
  work              optional; focus/lazy activity summary when relevant
```

`work` 形态：

```text
work
  relevant      true
  status        idle | queued | running | partial | blocked | complete | failed
  items[]       public work/task items relevant to the current response
    id          public work id, never closure id or extraction row id
    kind        focus | lazy_structural | lazy_dataflow | graph_refresh
    state       queued | running | complete | failed | cancelled | blocked
    scope       query | local | file | symbol
    reason      user/agent-readable reason for the background work
    progress    optional percent or done/total
```

不得再新增 `pending`、`pending_closures`、`pending_job_ids`、
`refinement_jobs`、focus-specific queue depth 等平行字段。已有 public
历史字段必须删除，语义并入 public `work`
envelope；但 `work` 只在与本次响应相关时出现。

### 10.5 v1.4.1 以来的架构收敛约束

v1.4.1 之后的清理目标不是单纯减少行数，而是把重复实现压回稳定边界。后续改动必须遵守以下模式：

- **先删除，再抽象**：零调用代码、过期别名、错误的 `#[allow(dead_code)]` 和未接入主路径的 facade 必须直接删除。只有当多个调用点共享同一不变式时才新增 helper、trait 或 struct；不得为了“参数成组出现”创建没有行为边界的状态对象。
- **入口层只做编排**：CLI、TUI、MCP 只解释参数、处理锁、进度、后台任务和用户可见错误。dirty check、stale cleanup、capability upgrade、precision downgrade guard、resolution、graph build 和 summary build 都必须走 engine/filesync/service 层的共享入口。
- **抽取层 helper 只承载机械一致性**：`languages::shared` 可以统一 `TextRange`、deterministic ID、`ScopeDef`、`BindingDef`、`ReferenceUse`、常见 `DataNode` 默认字段和 call-expression 查找。语言语义差异、特殊 AST 形状、return/callsite/field 规则必须留在各语言 adapter；禁止回到大型 `GenericExtractor`。
- **trait 默认实现只表达真正相同的规则**：如 `LanguageRuleKinds::validate_rule` 这类跨语言完全一致的校验可以进入 trait default；只要某语言的 rule kind、pattern、metadata 或展示名语义不同，就必须在 registry 中显式覆盖，而不是在默认实现里堆条件分支。
- **MCP analysis envelope 只有一个构建路径**：触发 lazy structural/dataflow、focus refinement 或 corpus 局部分析的 tool 响应必须通过 `AnalysisResponse` 等共享 builder 注入 `analysis`、`precision`、`coverage_counts`、`gaps`、`analysis_contract`、`query_id` 和 `QuerySnapshot`；`work` 由 builder 按“是否与本次响应相关”决定是否附带。`precision_tier`、`hint`、`lazy_diagnostics` 等旧 public 字段必须删除（**done: search.rs 已从 `precision_tier` 迁移到 `precision` + `Unavailable`→error, 2026-06-15**）；需要保留的低层诊断只能进入内部 debug 日志或显式 debug-only 工具。Graph、trace、search、context handler 不得手写同一 envelope，以免字段、status 或 retry 语义漂移。
- **public facade 改造以目标 API 为准**：快速原型期允许 breaking change。`atlas-engine` re-export 应保持调用者 ergonomics，但不得为了旧调用方式保留 wrapper、别名或过渡 API。`Internal / Prelude` re-export 可以服务 workspace 内部，但不得被文档描述为稳定外部 API。
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

## 11. Search、Context、MCP、CLI

### 11.1 Search
- FTS5 + LIKE fallback + fuzzy matching。
- `SearchQueryParser` 支持 `kind:`、`lang:`、`path:`、`name:` 前缀。
- MCP `search` 始终要求 `scope` 参数；scope 同时是搜索边界和 focus 热点。返回值必须声明该 scope 内结果是 complete 还是 partial，并通过既有 `work`/`tasks` 暴露后台 refinement。

### 11.2 Context
- 基于 symbol、callers/callees、file peers、importers/dependencies 构建 Agent context (Markdown)。
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
- 后续请求通过 `maybe_refresh_graph()` 检测外部索引变化（`ensure_structural_for_files` / `ensure_structural_for_symbol_name` 内部已调用，handler 无需重复）。
- 当 handler 内部触发 lazy structural 并写入新 facts（如 `symbol(view="context")` 的 Tier 3 解析），handler 显式调用 `force_refresh_graph()`（跳过缓存冷却），确保 graph 包含刚解析的边。
- `project(action="open")` 不索引，只同步激活项目；默认 `storage="auto"`，通过只读打开候选持久化库并复用 `project(status)` 的 index mode 判断，只有状态显示存在可复用索引时才复用 `.atlas/atlas.db`，否则使用内存库。
- 显式全项目索引只通过 CLI `atlas index` 执行。
- `search` 的 `scope` 永远强制参数；scope 同时是结果边界和 focus seed，即使存在 manual full index 也不省略。
- MCP 不支持 `background=true`；耗时提升通过 scoped focus/lazy 结果的 partial/diagnostics 和 `resume_query` 表达。
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

Trace/MCP lazy contract：
- MCP trace 入口必须优先通过 high-level `Engine`，由 engine 触发必要的 lazy dataflow；raw analysis consumer 不负责触发 lazy。
- 只要 lazy structural、lazy dataflow 或 focus refinement 被触发，响应就必须通过统一 public analysis view 暴露 `analysis`、`precision`、`gaps` 和 `analysis_contract`；只有当后台工作会影响本次 trace 结果时才附带 `work`。即使 trace 没有找到 path，也必须说明当前结果可用性、已知缺口和下一步动作；默认契约不暴露 `lazy_diagnostics`。
- CFG-consuming tools（如 `branch_diff`、`lifecycle`）如果已经 re-query 到 CFG 并基于 CFG 产出结果，`analysis_contract` 必须反映“本次工具已证明 CFG 可用”；不能同时返回 CFG 分析结果又声明 CFG 不可分析。
- `analysis_contract.safe_conclusions` 和 `unsafe_conclusions` 必须直接来源于 capability mask 或本次工具已验证的事实，不允许使用推测性默认值。

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

**branch_diff_semantic**：基于 `EffectComposition` 比较分支路径的语义效应差异，输出结构化 `BranchDiffIssue`（含 asymmetry kind、confidence、evidence）。MCP `branch_diff` tool 默认使用 semantic 路径（`semantic=true`）。

**lifecycle**：`transfer_state` 优先读取 `semantic_effects`（多效应按序处理），legacy 路径已移除。`FieldTransition` 按效应记录，DoubleFree/UseAfterFree 检测基于每次状态转换。

#### 12.3.2 多语言支持

`OwnershipContract` trait + `ConsumptionStyle` 区分 5 种消费模式（ExplicitCall/MethodCall/Implicit/Deferred/ContextManaged），`ResourceOpConfig::default_for(Language)` 通过 `producers`/`consumers` CalleeMatcher 覆盖 11+ 语言。每种语言混合使用多种消费风格（如 C 同时使用 `free()` 和 impliclit scope exit）。

#### 12.3.3 当前约束

- CFG effect annotation、field lifecycle 和 branch diff 先以 C/C++ 为主要适用语言。
- `FieldLifecycleEngine` 对字段状态做路径敏感分析，状态包括 `Unknown`、`MaybeLive`、`Assigned`、`Freed`、`Nullified`、`Escaped`、`Returned`、`Invalidated`。
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

- `atlas-engine` facade 目前已稳定，可作为独立 crate 被其他程序使用。
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

- **构建期间的并发读取**：当请求遇到处于 `AlreadyBuilding` 状态的 extraction job 时，它立即返回而不等待构建完成。同一 MCP 会话中的后续请求可能观察到过期数据。客户端应在短暂延迟后重试。

- **Include root auto-detection**: `project_root/include/` is auto-detected.
  Additional directories can be passed per-request via the `include_roots`
  MCP parameter.   Large C/C++ projects (e.g., the Linux kernel) may need project-specific
  include directories like `arch/<arch>/include/` or `include/generated/`.
  See MCP README for usage.

- **零初始语义**：`project(action="open")` 激活项目但不会索引它。在 search/trace 之前需要显式调用 `index`（manifest extraction）。没有 manifest 索引，lazy extraction 缺乏起点。

### Graph

- **Graph refresh after lazy extraction**: Production code uses
  `replace_files_in_place` (via `refresh_graph_for_files`) —
  old nodes/edges for changed files are removed, then fresh data
  is loaded from the store and merged.  For large change sets
  (> 500 files), falls back to full `GraphEngine::from_store()`
  rebuild.  `merge_delta_in_place` is an append-only helper
  used internally by `replace_files_in_place` for the merge step.

### Linux 增强

- **ResolutionSymbols 层**：仅 `EXPORT_SYMBOL` 标志被持久化。`initcall`/`module_init` 边和 `SYSCALL_DEFINE` diagnostics 仅持久化到完整的 `structural` 层（该层写入 `raw_edges`）。
