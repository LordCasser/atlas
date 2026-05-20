# 未来架构演进

本文只记录未完全落地或仍需增强的设计方向。当前实现状态见 [当前架构实现](./03-current-architecture.md)。

## 1. 近期优先级

1. 基于当前架构稳定变量来源追踪和调用路径查询所需 facts：bindings、binding uses、callsite args、data nodes、dataflow edges、function summaries、call graph。
2. 优先补齐 TypeScript/JavaScript/Python 的 backward slice 能力，再为 Java/C/C++/ArkTS/Cangjie 标注能力等级。
3. 建立“指定位置 -> 变量来源 -> caller path -> Agent evidence”的端到端 fixture。
4. 将 trace CLI/MCP 输出打磨为 Agent 可消费格式。
5. 在变量来源追踪和调用路径查询端到端测试完成前，不做 crate/workspace 拆分，不开启 Corpus 分支。

## 2. P5：变量来源追踪与调用路径查询 MVP

目标：

- 用户指定函数、问题变量、调用点、文件行列或代码模式。
- 从指定位置做 backward slice，追踪变量、表达式、字段访问、函数返回值、参数和 caller 实参来源。
- 结合 callers/callees 输出可能调用路径。
- 输出 path steps、源码 range、代码片段、confidence/provenance 和截断说明。
- 支持 CLI 和 MCP 查询，供 AI 继续分析。
- 不内置漏洞枚举、漏洞模式扫描、source/sink/sanitizer 规则或 finding 产出。

需要补齐：

- 从 file/line/column 定位 ReferenceUse、CallsiteArg、BindingUse、DataNode。
- BindingUse 扫描和 shadowing，避免同名变量误连。
- CallsiteArg 可靠落库，支持 argument index、keyword/named argument、receiver。
- DataFlowBuilder 从 capture 顺序启发式升级为 AST 结构建边。
- BackwardSlicer：从 DataNode/BindingUse 反向追 `Assign`、`FieldLoad`、`CallArg`、`Return`、`ArgToParam` 等来源边。
- CallerPathExplorer：沿 symbol call graph 向上游展开 caller chain，带 depth/limit/confidence。
- Trace output formatter：返回 JSON facts + 可读 Markdown evidence。

解析侧实现顺序：

1. 用 tree-sitter query 稳定捕获 definitions、references、call expressions、arguments、member/field access、return statements、assignments、imports。
2. LanguageAdapter 只做语言归一化，把 AST capture 转成统一 facts，不在 adapter 内做跨文件推理。
3. ScopeTree 和 LexicalBinder 先建立作用域、定义、使用和 shadowing，确保同名变量不会误连。
4. DataFlowBuilder 基于 AST 父子关系建边，避免只依赖 capture 顺序；同一语法结构必须能说明为何产生 `Assign`、`CallArg`、`Return` 或 `FieldLoad`。
5. CallsiteArg 必须从真实 call expression 生成，保留 receiver、callee text、argument index、named/keyword argument 和 range。
6. FunctionSummary 从函数内 facts 生成轻量摘要，只表达“参数/字段/调用结果/返回值之间可能有关”，不做漏洞语义判断。
7. Trace 查询在 query-time 组合 local dataflow、summary 和 call graph，按深度、预算和 confidence 截断。

MVP 语言端到端验收：

- TypeScript/JavaScript/Python 至少各有一个真实源码 fixture：指定位置实参 -> 局部变量 -> 字段访问表达式 -> caller path。
- Java/C/C++/ArkTS/Cangjie 至少提供 Level 1 callers/callees fixture，并为 Level 2/3 能力提供 best-effort fixture 或显式 unsupported diagnostics。
- CLI/MCP 或等价接口必须能读取 trace path，并提供 bounded 输出。
- 测试必须断言每一步的 kind、range、file、confidence/provenance。
- 失败或不支持的语言能力必须显式标记，不得静默通过。

## 3. 函数摘要与轻量跨函数追踪

当前 CFG 和 local dataflow 已有基础，但从指定位置回溯到 caller 仍需轻量函数摘要。不需要完整跨过程污点分析，也不规划 taint rule engine，先做 query-time summary 即可。

建议新增：

```text
FunctionSummary
  input_flows
  return_flows
  parameter_flows
  call_arg_flows
  field_flows
  side_effects
```

阶段目标：

1. intraprocedural summary：从参数到返回值、参数到关键 call arg、field/access path 到返回值。
2. callsite bridging：caller arg -> callee param、callee return -> caller assignment/call result。
3. limited backward interprocedural trace：深度限制、循环检测、confidence 衰减。
4. MCP `atlas_trace_point` / `atlas_trace_variable` 或扩展 `atlas_path` 支持 dataflow path。

## 4. Graph 分层加载

当前方向：

- SymbolGraph 可以全量 snapshot。
- DataFlowGraph、CFG、TraceGraph 应按函数、文件或 slice 按需加载。

原因：

- symbol-level graph 规模较小，适合常驻查询。
- dataflow/CFG 粒度更细，直接全量加载会增加内存压力。

演进目标：

- `GraphSnapshot` 只负责 symbol graph。
- dataflow/CFG 提供专门 reader 和 bounded traversal API。
- context/trace/path 工具按需加载局部 facts。

## 5. Engine / CLI / MCP 拆分

拆分时机：

```text
当前架构完成 MVP 语言 variable provenance / caller path E2E
  -> 拆分 engine / CLI / MCP
  -> 后续再分叉 Atlas 与 Corpus
```

拆分目标不是只抽 tree-sitter parser，而是抽出包含语法解析、变量来源追踪和调用路径查询能力的核心引擎：

```text
crates/
  atlas-engine
    types/facts
    parser/query extraction
    LanguageAdapter
    binding/dataflow/CFG builders
    resolution primitives
    variable provenance trace / slicing engine
    caller path explorer

  atlas-cli
    command-line interaction
    init/index/sync/search/context/trace commands

  atlas-mcp
    MCP transport
    tool routing
    output budgeting and formatting
```

拆分原则：

- `atlas-engine` 可以被其他 Rust 程序作为 crate 直接使用。
- `atlas-engine` 不依赖 CLI/MCP。
- CLI 和 MCP 只依赖 engine API。
- engine crate 包含变量来源追踪和调用路径查询，不把 trace 算法留在交互层。
- 不规划完整污点分析或 taint engine 产品线；变量来源追踪和调用路径查询是分析主线。
- 项目级持久化和索引策略可以先保留在 Atlas 应用层，后续为 Atlas/Corpus 分支分别适配。

不要过早拆分；当前门槛是 MVP 语言变量来源追踪和调用路径查询端到端测试完成。

## 6. 后续分叉方向

Engine / CLI / MCP 边界拆清后，演进分为两条线。

### 6.1 Atlas 分支：单仓库、单版本索引

目标：

- 做好当前 Atlas 主线：本地单仓库、单版本、workspace graph。
- 稳定 `.atlas/atlas.db`、GraphSnapshot、search/context/MCP。
- 强化 incremental sync、影响面分析、调用图、变量来源追踪和调用路径查询。
- 面向日常代码理解、审查、Agent 上下文构建和路径分析。

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

当前 MVP 语言按能力等级推进：

```text
Level 0: parse/index only, trace unsupported
Level 1: symbols + references + calls
Level 2: bindings + simple assignments
Level 3: field access + call args + returns
Level 4: CFG
Level 5: lightweight interprocedural summaries
```

当前能力边界和演进策略：

| 语言 | 当前边界 | 下一步重点 | 用户交互约束 |
|---|---|---|---|
| TypeScript | Level 3 主目标，Level 5 未完成 | 真实源码 fixture、函数摘要、caller arg -> callee param bridge | 展示 `capability_level=3`；跨函数结果标注 summary/best-effort |
| JavaScript | Level 3 主目标，沿 TS JS grammar | 与 TS 共用 dataflow/query 但保持 language 独立 | 展示 `language=javascript`，不要混用 TS 名称 |
| Python | Level 3 主目标，动态语义低置信度 | import alias、attribute access、keyword args、returns fixture | 动态属性、反射调用、monkey patch 输出 limitation |
| Java | Level 1 当前保底，Level 2/3 待 fixture 约束 | method invocation、argument、return、package/import resolution | caller/callee 可用；变量来源不可用时明确 unsupported |
| C | include-aware Level 1/2 best-effort | callsite args、局部 assignment、return、include provenance | 宏、函数指针、复杂指针别名标 low confidence/unsupported |
| C++ | include-aware Level 1/2 best-effort | method call、namespace、simple assignment、return | 模板、重载、ADL 不得宣称精确 |
| ArkTS | TypeScript grammar fallback 的 Level 1/2 best-effort | ArkTS fixture、语言标识、TS fallback provenance | 输出必须说明 ArkTS via TS grammar fallback |
| Cangjie | Level 0/1 minimal | grammar spike、minimal adapter、definitions/calls fixture | trace 默认 unsupported，不返回静默空结果 |

交互层演进：

- `atlas status` / `atlas doctor` 应展示当前项目各语言的 capability profile 汇总。
- `atlas trace` 在结果头部展示 `language`、`capability_level`、`supported_features`、`unsupported_features`、`limitations`。
- MCP trace/query/context 工具必须在 JSON 顶层返回 `capability` 对象，Agent 不需要额外猜测语言能力。
- 当请求超过语言边界时，返回 partial result 和 diagnostics；例如能返回 callers 但不能返回变量来源时，应明确说明 `local_provenance unsupported for java`。

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
- `atlas_trace_variable`
- `atlas_trace_point`

所有工具必须保持：

- bounded output
- project path validation
- explicit confidence/provenance
- structured JSON plus Markdown context where useful

## 10. 不建议近期投入

- 完整编译器级 C/C++ 语义。
- 完整 Java classpath/build system 解析。
- Python 动态类型精确推断。
- 全语言 framework resolver 生态。
- 提前构建大型 graph database。
- 在 MVP 语言变量来源追踪和调用路径查询端到端测试完成前做 workspace 大拆分。
- 在 engine/CLI/MCP 边界拆清前开启 Corpus 分支。
