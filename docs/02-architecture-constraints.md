# Atlas 架构约束

本文记录实现和重构必须遵守的不变式。当前代码可以逐步演进，但不能违反这些边界。

## 1. 总体原则

1. Atlas 是 CodeGraph-inspired，不是 CodeGraph-compatible。
2. Rust 实现应使用 trait、newtype ID、enum、immutable facts、batch write、read snapshot 和 Rayon，而不是照搬 TypeScript 中心类结构。
3. SQLite 是持久化源；内存图只作为查询加速和分析工作集。
4. MCP 是一等入口；CLI、MCP、context 输出都必须可限制大小。
5. 所有启发式语义结果必须可解释，不能把低置信度结果伪装成精确结果。

## 2. 模块边界

当前逻辑分层：

```text
CLI / MCP
Context / Graph / Search / Sync
Analysis
Resolution
Extraction
Database
Types
```

约束：

- `types` 不依赖上层模块。
- `db` 负责 schema、读写、迁移，不承载语言语义规则。
- `extraction` 只产出单文件事实，不做跨文件 resolution。
- `resolution` 更新 resolved facts，不直接承担展示格式。
- `graph` 从 resolved facts 和 structural facts 构建 symbol graph。
- `analysis` 消费 dataflow、CFG 和 call graph，不破坏底层 facts。
- `cli` 和 `mcp` 只编排能力，不内嵌解析、resolution 或分析算法。

当前阶段约束：

- 先在当前单 crate/当前架构内完成变量来源追踪与调用路径查询端到端测试。
- 在端到端 trace 能力稳定前，不先做 workspace/crate 拆分。
- CLI 和 MCP 可以继续在同一 crate 内编排，但新增逻辑要维持清晰边界，避免后续拆分时把交互层和引擎层绑死。

## 3. ID 约束

所有持久化 ID 必须 deterministic，禁止 UUID/自增作为核心身份。

推荐哈希输入：

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

注意：

- `ReferenceId` 必须包含 `ReferenceKind`，避免同 range 的 call/field captures 冲突。
- 不得用 line number 作为稳定 ID 核心。
- ID 类型必须分层，不能用 `SymbolId::default()` 伪装 dataflow node。

## 4. 抽取约束

抽取层采用：

```text
tree-sitter parser
  -> per-language .scm queries
  -> LanguageAdapter normalization
  -> FileFacts
```

约束：

- 不实现大型 `GenericExtractor`。
- LanguageAdapter 不填跨文件语义结果。
- Adapter 不手写重复的 enclosing function/source_symbol 逻辑；source、scope、binding 由 binder 统一处理。
- 单文件失败必须结构化记录，不中断项目索引。
- ArkTS MVP 复用 TypeScript grammar，但 language 必须是 `arkts`。
- C/C++ 是 best-effort，不承诺完整 preprocessing、模板、重载。
- Cangjie 不属于 MVP，必须显式启用 `cangjie` feature；启用前不参与默认发现、默认编译或 `all-languages` 验收。

## 5. Fact 模型约束

`FileFacts` 是抽取层对外边界，包含：

```text
file
symbols
scopes
references
imports/exports
raw structural facts
callsites
bindings/binding_uses
data_nodes/dataflow_edges
cfg_nodes/cfg_edges
diagnostics
```

不变式：

- 同一 `FileFacts` 中的 facts 必须属于同一个 file。
- range 必须包含 byte offset 和 line/column。
- references 永不因为 resolved 而删除。
- unresolved references 必须保留。
- callsite 必须能回溯到 reference location。
- dataflow 使用 `DataNodeId -> DataNodeId`。
- CFG 节点必须属于同一 function，函数 CFG 应有 Entry/Exit。

## 6. 语言能力边界约束

不同语言的实现精度必须由显式 capability profile 描述，不能由 CLI/MCP 在展示层临时猜测。

推荐模型：

```text
LanguageCapabilityProfile
  language
  capability_level
  supported_features
  unsupported_features
  known_limitations
  required_facts
  confidence_floor
  test_coverage_marker
```

约束：

- capability profile 属于 engine/analysis 边界的一部分，CLI/MCP/context 只能读取并展示，不能绕过它宣称能力。
- 每个 trace、callgraph、context 查询结果都必须携带实际使用的语言能力信息。
- 查询请求超出当前语言边界时，analysis 应返回 partial result + diagnostics，而不是让交互层用空数组表达失败。
- `unsupported_features` 必须能区分“尚未实现”“语言语义本身难以精确”“缺少当前项目 facts”三类原因。
- 低置信度 fallback 必须带 `confidence`、`strategy` 和 `provenance`，不得升级为精确结果。
- 新增或提升某语言能力等级时，必须同时更新 capability profile、fixture、测试规范和用户可见输出样例。

## 7. Persistence 约束

`.atlas/atlas.db` 不兼容 `.codegraph`。

必须保留的事实类别：

- files
- symbols
- scopes
- references
- imports
- symbol_edges
- callsites
- bindings / binding_uses
- data_nodes / dataflow_edges (6 字段完整 TextRange)
- cfg_nodes / cfg_edges
- project metadata
- FTS indexes
- schema versions

约束：

- SQLite 使用 WAL。
- 写路径走事务和 batch write。
- 读路径可以短连接或 read API。
- 快速开发阶段 `CURRENT_SCHEMA_VERSION` 保持为当前 schema 代号；schema 变化必须同步更新当前架构文档和测试。部署迁移和旧库兼容不作为当前约束。
- symbol graph 与 dataflow graph 必须分表。

## 8. Resolution 与 Graph 约束

- `ReferenceResolver` 只产生 resolved facts。
- `GraphBuilder` 负责从 resolved references、callsites、raw structural facts 生成 symbol-level edges。
- `GraphSnapshot` 发布后不可变。
- Sync 只有在写事务成功后刷新 snapshot。
- 删除或修改文件时必须失效相关 references 和 edges，避免悬空 resolved target。
- MCP 默认可过滤低置信度边，但要允许显式 include low confidence。

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

## 9. 引擎拆分边界

完成 MVP 语言变量来源追踪与调用路径查询端到端测试后，再拆分可复用引擎 crate。

拆分目标：

```text
atlas-engine
  core types and facts
  tree-sitter parser/query extraction
  language adapters
  binding/dataflow/CFG builders
  reference resolution where storage-independent
  variable provenance trace / slicing engine

atlas-cli
  command-line interaction
  project initialization
  indexing/sync commands
  search/context/trace commands

atlas-mcp
  JSON-RPC stdio transport
  MCP tool routing
  output budgeting and formatting
```

约束：

- `atlas-engine` 必须可作为独立 crate 被其他程序直接使用。
- `atlas-engine` 不能依赖 CLI 参数解析、MCP transport 或交互格式。
- CLI/MCP 只能调用 engine API，不应反向被 engine 调用。
- engine crate 必须包含语法解析、facts 构建和变量来源追踪与调用路径查询能力，而不是只包含 tree-sitter parser。
- 不在 engine crate 中规划完整自动扫描规则引擎；trace/slicing 是分析主线。
- 持久化和项目索引可以先保留在 Atlas 应用层，后续按 Atlas/Corpus 两个分支的存储模型分别演进。

## 10. Corpus 边界

大型多版本源码索引系统不并入 Atlas 主体。

Corpus 只能在引擎 crate 拆分之后开启。届时架构分叉为：

```text
atlas-engine
  -> atlas workspace graph: 单仓库、单版本、本地索引
  -> corpus: 大型项目、多版本、Git blob/tag 索引
```

Corpus 的详细功能和需求以历史文档为准，需要时从 git 历史恢复旧的 Corpus/Elixir 分析文档。当前约束只保留边界：

- Corpus 不共享 Atlas 的 path-based 持久化 ID。
- Corpus 以 Git blob/tag/path/version mapping 为核心索引模型。
- Corpus 可以复用 engine 的 parser、LanguageAdapter、函数/符号/引用抽取和变量来源追踪与调用路径查询能力。
- Corpus 必须独立选择存储、索引、Web/API 和 MCP 工具语义。
