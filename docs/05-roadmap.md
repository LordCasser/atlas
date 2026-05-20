# 未来架构演进

本文只记录未完全落地或仍需增强的设计方向。当前实现状态见 [当前架构实现](./03-current-architecture.md)。

## 1. 近期优先级

1. 基于当前架构稳定 schema v7 和 taint 基础写入/查询。
2. 补齐 MVP 语言的 lexical binding、local dataflow 和 taint 所需 facts。
3. 为每种 MVP 语言建立污点分析端到端 fixture。
4. 将 taint CLI/MCP 输出打磨为 Agent 可消费格式。
5. 在污点端到端测试完成前，不做 crate/workspace 拆分，不开启 Corpus 分支。

## 2. P5：污点分析 MVP

目标：

- 从 YAML 规则识别 source、sink、sanitizer、propagator。
- 基于 dataflow graph 做 source-to-sink propagation。
- 输出 finding、severity、confidence、source/sink、path steps。
- 支持 CLI 和 MCP 查询。

需要补齐：

- `.atlas/rules/*.yaml` 用户规则覆盖和合并策略。
- rule validation 和错误报告。
- finding persistence/query API。
- path 输出中的代码片段和上下文预算。
- sink/source 匹配从简单 substring 逐步增强为 qualified name、access path、callee、argument index。

MVP 语言端到端验收：

- 每种 MVP 语言至少有一个 source -> dataflow propagation -> sink 的 fixture。
- fixture 应验证 finding、severity、confidence、source range、sink range、path steps。
- CLI `atlas taint` 必须能在 fixture 项目上输出稳定结果。
- MCP 或等价接口必须能读取 finding/path，并提供 bounded 输出。
- 失败或不支持的语言能力必须显式标记，不得静默通过。

## 3. 函数摘要与跨过程数据流

当前 CFG 和 local dataflow 已有基础，但跨函数传播仍需函数摘要。

建议新增：

```text
FunctionSummary
  input_flows
  return_flows
  sink_flows
  source_flows
  sanitizer_flows
  side_effects
```

阶段目标：

1. intraprocedural summary：从参数到返回值、参数到 sink、source 到返回值。
2. callsite bridging：arg -> param、return -> call result。
3. limited interprocedural propagation：深度限制、循环检测、confidence 衰减。
4. MCP `atlas_taint_trace` 或扩展 `atlas_path` 支持 dataflow path。

## 4. Graph 分层加载

当前方向：

- SymbolGraph 可以全量 snapshot。
- DataFlowGraph、CFG、TaintGraph 应按函数、文件或 slice 按需加载。

原因：

- symbol-level graph 规模较小，适合常驻查询。
- dataflow/CFG 粒度更细，直接全量加载会增加内存压力。

演进目标：

- `GraphSnapshot` 只负责 symbol graph。
- dataflow/CFG 提供专门 reader 和 bounded traversal API。
- context/taint/path 工具按需加载局部 facts。

## 5. Engine / CLI / MCP 拆分

拆分时机：

```text
当前架构完成 MVP 语言 taint E2E
  -> 拆分 engine / CLI / MCP
  -> 后续再分叉 Atlas 与 Corpus
```

拆分目标不是只抽 tree-sitter parser，而是抽出包含语法解析和污点分析能力的核心引擎：

```text
crates/
  atlas-engine
    types/facts
    parser/query extraction
    LanguageAdapter
    binding/dataflow/CFG builders
    resolution primitives
    taint rules and taint engine

  atlas-cli
    command-line interaction
    init/index/sync/search/context/taint commands

  atlas-mcp
    MCP transport
    tool routing
    output budgeting and formatting
```

拆分原则：

- `atlas-engine` 可以被其他 Rust 程序作为 crate 直接使用。
- `atlas-engine` 不依赖 CLI/MCP。
- CLI 和 MCP 只依赖 engine API。
- engine crate 包含污点分析，不把 taint 留在交互层。
- 项目级持久化和索引策略可以先保留在 Atlas 应用层，后续为 Atlas/Corpus 分支分别适配。

不要过早拆分；当前门槛是 MVP 语言污点分析端到端测试完成。

## 6. 后续分叉方向

Engine / CLI / MCP 边界拆清后，演进分为两条线。

### 6.1 Atlas 分支：单仓库、单版本索引

目标：

- 做好当前 Atlas 主线：本地单仓库、单版本、workspace graph。
- 稳定 `.atlas/atlas.db`、GraphSnapshot、search/context/MCP。
- 强化 incremental sync、影响面分析、调用图和 taint 分析。
- 面向日常代码理解、审查、Agent 上下文构建和安全分析。

该分支继续使用 project-relative path 作为文件身份基础。

### 6.2 Corpus 分支：大型项目多版本索引查询

目标：

- 面向 Linux / U-Boot / BusyBox 等大型多版本 Git 项目。
- 支持同时索引大量 tags/releases。
- 使用 Git blob 去重，避免同一文件内容跨版本重复解析。
- 建立 version -> blob/path 映射和 version bitmap/postings。
- 支持函数实现查询、identifier definitions/references、跨版本函数 diff、first-seen/timeline。
- 提供类似 Elixir 的 Web/API，以及面向 Agent 的 Corpus MCP。

Corpus 输入与索引模型：

- 输入来源是 bare Git repository + remotes + tags。
- 版本边界来自 Git tags。
- 核心索引单位是 Git blob，不是工作区 path。
- 同一 blob 可在多个版本和路径中复用。
- 存储可从 SQLite metadata 起步，后续演进到 bitmap、compressed postings、mmap segment files 等。

Corpus 详细需求曾在旧文档中展开；当前 docs 清理后，如需恢复细节，从 git 历史查看旧的架构变动文档和 Elixir 分析文档。

## 7. 语言支持演进

MVP 之后可以引入：

- Go
- Rust
- C#
- PHP
- Ruby
- Swift
- Kotlin

新增语言必须满足：

- adapter 和 query 独立。
- fixtures 覆盖 definitions/imports/calls。
- 不修改中心 mega-extractor。
- resolution 规则可插拔。

## 8. Search 与 Context 演进

后续增强：

- 更强的 hybrid ranking：FTS、qualified name、graph centrality、path relevance、kind bonus。
- natural language query 到 symbol-like terms 的稳定抽取。
- import/export node 自动归约到 definition。
- context 输出加入 relationship map、confidence warnings、truncation note。
- test/non-production downrank 和 per-file diversity cap。

## 9. MCP 演进

可新增或增强：

- `atlas_usages`
- `atlas_dependencies`
- `atlas_dependents`
- `atlas_dataflow_path`
- `atlas_taint_trace`
- `atlas_rules`

所有工具必须保持：

- bounded output
- project path validation
- explicit confidence/provenance
- structured JSON plus optional Markdown context

## 10. 不建议近期投入

- 完整编译器级 C/C++ 语义。
- 完整 Java classpath/build system 解析。
- Python 动态类型精确推断。
- 全语言 framework resolver 生态。
- 提前构建大型 graph database。
- 在 MVP 语言污点端到端测试完成前做 workspace 大拆分。
- 在 engine/CLI/MCP 边界拆清前开启 Corpus 分支。
