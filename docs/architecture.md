# Atlas 架构文档

本文是 Atlas 的**单一权威架构文档**，合并了架构约束、当前实现状态、已落地的设计决策（Lazy Index、DataflowFull 摘要层）。本文对代码实现有约束力；当代码与本文冲突时，以本文为准并同步修正代码。

## 1. 总体原则

1. Atlas 是 CodeGraph-inspired，不是 CodeGraph-compatible。
2. Rust 实现使用 trait、newtype ID、enum、immutable facts、batch write、read snapshot 和 Rayon。
3. SQLite 是持久化源（`.atlas/atlas.db`）；内存图只作为查询加速和分析工作集。
4. MCP 是一等入口；CLI、MCP、context 输出都必须可限制大小。
5. 所有启发式语义结果必须可解释，不能把低置信度结果伪装成精确结果。

## 2. 模块边界与依赖方向

### 2.1 Crate 结构

项目是 13 个 Cargo package 的 workspace：

```text
crates/
  atlas-engine/        facade crate，re-export types/db/extraction/resolution/graph/analysis/search/context/filesync/lazy
    crates/types/      ID、enum、IR、binding、dataflow、CFG、trace 查询类型、capability profiles
    crates/workspace/  ProjectRoot、WorkspacePaths、SourcePath
    crates/db/         SQLite schema v3、Store API、readers、schema 迁移基础设施
    crates/extraction/ tree-sitter 解析、query、scope、semantic binder、lexical binder、dataflow、CFG、worker pool
    crates/resolution/ builtin filter、scope/container/import/include/name matching、PathAliasResolver
    crates/graph/      GraphBuilder、GraphSnapshot、GraphEngine
    crates/analysis/   变量来源追踪与调用路径查询、SummaryBuilder、CrossFunctionBridge
    crates/search/     FTS5、LIKE/fuzzy、query parser、scoring
    crates/context/    Agent context builder (Markdown)
    crates/filesync/   file discovery、change detection、file lock、watcher
    crates/lazy/       Lazy dataflow engine — on-demand analysis with budget caps
  atlas-mcp/           MCP server (rmcp stdio JSON-RPC)、24 tools
  atlas-cli/           CLI binary + commands + integration tests
```

### 2.2 依赖方向（严格无环）

```text
atlas-cli → atlas-engine, atlas-mcp
atlas-mcp → atlas-engine
atlas-engine → types, workspace, db, extraction, resolution, graph, analysis, search, context, filesync, lazy
filesync → graph, resolution, extraction, db, types, workspace
search / context → graph, db, types
analysis → db, types, workspace
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
- 所有 14 种语言均接入 `all-languages` feature set。

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

## 6. Persistence 约束

### 6.1 Schema（当前版本：V1）

当前 schema 版本为 V1，所有变更直接在主 DDL 中进行，无需迁移。

主要表（19 张）：

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
| `analysis_artifacts` | lazy dataflow/CFG 追踪 |
| `file_index_layers` | 每文件每层索引状态 |
| `project_metadata` | 项目级键值配置 |
| `symbols_fts` | FTS5 符号名索引 |

约束：
- SQLite 使用 WAL。
- 写路径走事务和 batch write。
- 读路径可以短连接或 read API。
- symbol graph 与 dataflow graph 必须分表。
- `dataflow_edges` 保持纯 intra-procedural；跨函数事实仅存在于摘要表。

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

analysis 层按需加载 dataflow facts（而非全量预加载），通过 `LazyWindow` 控制分析范围，budget-capped (25s/64 units)。`ExtractionMode::LazyDataflow` 支持增量按需抽取。

### 7.2 跨函数桥接（DataflowFull）

Schema V3 实现了持久化摘要层：

```
dataflow_edges    = intra-procedural, fine-grained, direct edges (不变)
function_summary  = intra-procedural, transitive-closure, per-function (V3 新增)
trace             = inter-procedural, by composing summaries along call graph
```

- `SummaryBuilder` 从 dataflow_edges BFS 计算函数摘要。
- `SummaryStore` 持久化 4 张摘要表，支持全量构建（`build_all`）和增量构建（`build_for_function`）。
- `CrossFunctionBridge` 替代了旧的 runtime `SummaryEdgeProvider`，实现 ArgToParam 和 ReturnToCall 桥接。
- 向后兼容：旧 DB 无摘要表时降级为 runtime BFS。
- 增量失效：sync 时删除受影响函数的摘要行并重建。

## 8. Resolution 与 Graph 约束

- `ReferenceResolver` 只产生 resolved facts。
- `GraphBuilder` 从 resolved references、callsites、raw structural facts 生成 symbol-level edges。
- `GraphSnapshot` 发布后不可变；Sync 只有在写事务成功后刷新 snapshot。
- 删除或修改文件时必须失效相关 references 和 edges。

置信度分层：

```text
1.00 compiler/LSP/SCIP exact, if future supported
0.95 same-scope exact / exact qualified name
0.90 import/package exact
0.80 framework convention / namespace proximity
0.70 same-file or same-package name match
0.50 fuzzy / ambiguous fallback
<0.50 unresolved or speculative
```

## 9. 语言能力边界

### 9.1 模型

```text
LanguageCapabilityProfile
  language
  capability_level       → None / Symbolic / DataflowBasic / DataflowFull
  supported_features     → 向后兼容的字符串列表
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

### 10.1 Lazy Index（三阶段演进，全部已实现）

**P0: Scope Index** — 允许 `--include`/`--scope`/`--exclude` 限制索引范围，降低大型项目 index 时间和 DB 体积。

**P1: Manifest Extraction** — `ExtractionMode::Manifest`：仅提取顶层符号，为 lazy structural 提供候选源。通过 `symbols.layer` 字段区分。

**P2: Lazy Structural** — 查询时按需触发完整 structural extraction。`LazyStructuralService` + `CandidateProvider` + `StructuralLoader`。

## 11. Search、Context、MCP、CLI

### 11.1 Search
- FTS5 + LIKE fallback + fuzzy matching。
- `SearchQueryParser` 支持 `kind:`、`lang:`、`path:`、`name:` 前缀。
- MCP `search` 要求 `scope` 参数（manifest-only 索引时），small scope 触发 bounded structural parsing，large scope 返回 manifest-level 结果 + narrowing warning。

### 11.2 Context
- 基于 symbol、callers/callees、file peers、importers/dependencies 构建 Agent context (Markdown)。

### 11.3 MCP
- 基于 `rmcp` 的 stdio JSON-RPC transport。
- **24 个短名工具**（无 `atlas_` 前缀）：

| 组 | 工具 |
|----|------|
| 项目管理 | `open_project`, `index`, `status`, `files`, `language_capabilities` |
| 符号搜索 | `search`, `symbol`, `usages` |
| 图导航 | `neighbors`, `callers`, `callees`, `callgraph`, `path`, `explore`, `impact` |
| 上下文 | `context` |
| Trace | `trace_point`, `trace_variable`, `trace_caller_path`, `trace_forward` |
| 文件依赖 | `dependencies`, `dependents` |
| 后台任务 | `task_status`, `wait_for_task` |

- Graph 惰性初始化：首次 graph-backed tool 调用时构建 snapshot。
- `open_project` 不索引，只激活项目；调用后需单独 `index`。
- `search` 的 `scope` 对 manifest-only 索引为强制参数；存在 manual full index 时为可选。
- `background: true` 支持：`search`, `index`, `open_project`。
- 结果截断 25KB，额外 content block 标注截断信息。

### 11.4 CLI
核心命令：`init`, `index`, `sync`, `status`, `doctor`, `files`, `search`, `context`, `trace`, `mcp`。

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

- `trace_point` — 解析源码位置到 full context。
- `trace_variable` — backward dataflow walk 获取变量来源。
- `trace_caller_path` — backward call edge walk 获取调用者链路（单链）。
- `trace_forward` — forward call edge walk 回答"how does A reach B"。

### 12.2 输出契约

所有 trace 工具返回 `TraceQueryResponse<T>` envelope：
- `ok`, `kind`, `capability`, `partial_result`, `diagnostics`, `result`。
- 详见 [`trace-contract.md`](./trace-contract.md)。

## 13. Cargo Features

| 层级 | Features |
|------|----------|
| 默认 | `typescript`, `javascript`, `python` |
| MVP | + `java`, `c`, `cpp`, `arkts` |
| `all-languages` | + `go`, `csharp`, `rust`, `php`, `ruby`, `kotlin`, `cangjie` |
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
- 性能基线：[`performance.md`](./performance.md)

## 16. 维护规则

1. 本文是架构的单一权威来源。当模块边界、persistence 规则、ID 规则、capability profiles 或 schema 版本变化时，同步更新本文。
2. 新增语言、新增 schema 表、新增 CLI/MCP 工具、新增 analysis 能力时，同步更新能力表和对应章节。
3. 不再保留独立的架构约束、当前状态、或临时设计文档；所有架构信息统一于此文。
4. 删除的文档不再保留归档副本。
