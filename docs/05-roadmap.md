# 未来架构演进

本文只记录未完全落地或仍需增强的设计方向。当前实现状态见 [当前架构实现](./03-current-architecture.md)。

## 1. 近期优先级

1. 在当前 `atlas-engine` facade + engine 内部 crates + CLI/MCP 的 workspace 内继续稳定变量来源追踪和调用路径查询所需 facts：bindings、binding uses、callsites、inline call arguments、data nodes、dataflow edges、call graph；`callsite_args` 表已移除，调用实参统一使用 `callsites.args_json` + call-arg DataNode；FunctionSummary 已实现 query-time 基础版。
2. TypeScript/JavaScript/Python 已有 backward slice 端到端测试，下一步重点是把启发式 dataflow 转成更受 AST 结构约束的事实，并收紧断言。
3. Java/C/C++/ArkTS 以及 Go/Rust/C#/PHP/Ruby/Kotlin 当前 capability profile 已声明 `DataflowBasic`，但仍是“基础局部 dataflow + 明确 limitation”的阶段；不得把它们描述成完整跨函数变量来源追踪、CFG 或编译器级语义。Bash/Cangjie 只在显式启用时展示 experimental capability。
4. 继续打磨 trace CLI/MCP 输出，使 capability、partial result、diagnostics、confidence/provenance 对 Agent 足够明确。
5. `atlas-engine` facade crate 已存在；后续重点不是再做大拆分，而是稳定 facade public API、CLI/MCP 契约和 trace 语义。Corpus 仍不启动。

## 2. V1 生产发布前必做

当前代码已经能构建本地索引、提供 CLI/MCP 查询和基础 trace，但离第一版生产发布仍缺少一组“可交付性”工作。下面清单优先级高于新语言和新分析能力。

### 2.1 用户文档与发布文档

- 完成面向用户的安装文档：release binary、源码构建、feature 选择、最低 Rust 版本、macOS/Linux/Windows 已验证矩阵。
- 完成首个项目 quickstart：`atlas init`、`atlas index`、`atlas search`、`atlas trace ... --json`、`atlas mcp` 的完整可复制流程，并标明 MCP 读取已有 `.atlas/atlas.db`，不会自动 index。
- 完成 MCP 客户端文档：Claude Desktop、Cursor、Continue/VS Code 的配置、重启/刷新方式、stdio 日志位置、常见 JSON-RPC 错误。
- 完成 capability 用户说明：每种语言的 `capability_level`、支持/不支持功能、confidence_floor 和 limitation，不能只写“支持语言”。
- 完成 trace 输出解读文档：`TraceQueryResponse`、`partial_result`、diagnostics、range、evidence、confidence/provenance 的含义，给出成功、partial、unsupported 三类样例。
- 完成故障排查文档：未初始化项目、数据库 schema 过新/过旧、未先 index、路径不在 project root、feature 未编译、MCP 输出被截断。
- 发布前写清楚非目标：不是漏洞扫描器、不是 taint engine、不是编译器/LSP 替代品、不是多版本 Corpus。

### 2.2 MCP 生产化

- 固化 MCP 工具命名策略。当前已注册 19 个工具：Atlas 专属工具使用 `atlas_` 前缀，通用语义工具 `usages`、`dependencies`、`dependents` 保持无前缀；V1 前只需在工具契约中明确这条规则。
- 为全部 MCP 工具补齐 schema、正常调用、错误调用、bounded output 和 project path validation 测试；trace/context/search 还必须断言 capability/confidence/provenance。
- 统一 MCP 错误模型：工具参数错误、项目未索引、路径越界、无结果、能力不支持要区分 `isError` 与 JSON 内部 diagnostics。
- 明确 graph snapshot 刷新语义。当前工具路由有 `maybe_refresh_graph()`，但 `call_tool()` 注释说明默认不逐请求重建；V1 需要决定是显式刷新、status 刷新还是要求重启 MCP，并写入用户文档。
- 为 MCP 增加机器可读版本信息：Atlas 版本、schema version、tool contract version、compiled features，供 Agent 做兼容判断。

### 2.3 Trace 与语言能力收敛

- 对所有 `DataflowBasic` 语言建立最小 path-level smoke：真实源码 -> index -> trace query -> path steps/range/confidence/provenance 断言。当前 README/roadmap 必须避免把基础 dataflow 等同于完整变量来源追踪。
- 收紧 TS/JS/Python 的断言：每一步 kind、range、file、confidence/provenance、truncation 都要被测试约束。
- Java/C/C++/ArkTS/Go/Rust/C#/PHP/Ruby/Kotlin 需要按语言补齐 capability snapshot、golden fixtures、dataflow smoke，并明确哪些结构仍 partial/low-confidence。
- Bash/Cangjie 保持 opt-in experimental，补最小 doctor/capability/unsupported trace 测试，避免用户误认为默认可用。
- FunctionSummary 继续保持 query-time 基础版；V1 前只在文档中宣称“有限摘要/非完整跨函数传播”，除非已有跨函数参数/返回路径 fixture 约束。

### 2.4 CLI 与数据库发布门槛

- `atlas doctor` 应输出 release 诊断所需信息：schema status、compiled features、language capability、SQLite/FTS 可用性、项目是否已 index。
- Schema 迁移基础设施已存在，`MIGRATIONS` 当前为空（V1）。V1 前必须决定发布兼容策略：只支持 V1 fresh DB，还是承诺后续 V1->V2 自动迁移；该策略必须写入 README 和 doctor 输出。
- 确认所有读源码片段的路径都限制在 project root 内，并有测试覆盖。
- 确认默认输出和 JSON 输出都有稳定边界，长结果必须带 truncation 信息。
- 建立 release smoke：`cargo test`、`cargo test -p atlas-cli --features all-languages`、`cargo test -p atlas-cli --features mcp`、`cargo test -p atlas-cli --features "all-languages,mcp"`，以及 release binary 对样例项目的 init/index/search/trace/mcp smoke。

### 2.5 项目元数据与交付

- 补齐版本号、CHANGELOG/release notes、许可证确认、目标平台说明。
- 明确 `.atlas/` 目录的兼容性和清理/重建方式。
- 给出性能基线：小/中/大项目的索引时间、数据库大小、MCP 查询延迟、内存占用，并说明已验证的规模上限。
- 发布前冻结 Trace Contract v1 和 MCP tool contract v1；后续破坏性变更必须升级 contract version。

## 3. P5：变量来源追踪与调用路径查询 MVP

目标：

- 用户指定函数、问题变量、调用点、文件行列或代码模式。
- 从指定位置做 backward slice，追踪变量、表达式、字段访问、函数返回值、参数和 caller 实参来源。
- 结合 callers/callees 输出可能调用路径。
- 输出 path steps、源码 range、代码片段、confidence/provenance 和截断说明。
- 支持 CLI 和 MCP 查询，供 AI 继续分析。
- 不内置漏洞枚举、漏洞模式扫描、漏洞规则系统或 finding 产出。

需要补齐：

- 从 file/line/column 定位 ReferenceUse、BindingUse、DataNode，以及当前 callsite inline argument / call-arg DataNode。
- BindingUse 扫描和 shadowing，避免同名变量误连。
- 统一调用实参事实源已实现：`callsites.args_json` + call-arg `DataNode` 为单一事实源；`callsite_args` 表已移除。
- DataFlowBuilder 从 capture 顺序启发式升级为 AST 结构建边。
- BackwardSlicer：从 DataNode/BindingUse 反向追 `Assign`、`FieldLoad`、`CallArg`、`Return`、`ArgToParam` 等来源边。
- CallerPathExplorer：沿 symbol call graph 向上游展开 caller chain，带 depth/limit/confidence。
- Trace output formatter：返回 JSON facts + 可读 Markdown evidence。

解析侧实现顺序：

1. 用 tree-sitter query 稳定捕获 definitions、references、call expressions、arguments、member/field access、return statements、assignments、imports。
2. LanguageAdapter 只做语言归一化，把 AST capture 转成统一 facts，不在 adapter 内做跨文件推理。
3. ScopeTree 和 LexicalBinder 先建立作用域、定义、使用和 shadowing，确保同名变量不会误连。
4. DataFlowBuilder 基于 AST 父子关系建边，避免只依赖 capture 顺序；同一语法结构必须能说明为何产生 `Assign`、`CallArg`、`Return` 或 `FieldLoad`。
5. 调用实参 facts 必须从真实 call expression 生成，保留 receiver、callee text、argument index、named/keyword argument 和 range；调用实参事实源已统一为 `callsites.args_json` + call-arg DataNode。
6. FunctionSummary 已实现 query-time 基础版（`SummaryBuilder`），从函数内 facts 生成轻量摘要（parameter reachability via BFS），跨函数传播仍硬依赖 dataflow_edges；不做漏洞语义判断。
7. Trace 查询当前组合 local dataflow 和 call graph；引入 summary 后再按深度、预算和 confidence 做跨过程截断。

MVP 语言端到端验收：

- TypeScript/JavaScript/Python 至少各有一个真实源码 fixture：指定位置实参 -> 局部变量 -> 字段访问表达式 -> caller path。
- Java/C/C++/ArkTS 至少提供 callers/callees fixture，并为 Level 2/3 DataflowBasic 能力提供 best-effort fixture、path-level smoke 或显式 partial/low-confidence diagnostics；Cangjie 不进入 MVP fixture 门槛。
- CLI/MCP 或等价接口必须能读取 trace path，并提供 bounded 输出。
- 测试必须断言每一步的 kind、range、file、confidence/provenance。
- 失败或不支持的语言能力必须显式标记，不得静默通过。

## 4. 函数摘要与轻量跨函数追踪

当前 CFG 和 local dataflow 已有基础；从指定位置回溯到 caller 现在主要依赖调用图和有限的参数/调用点事实。轻量函数摘要已有 query-time 基础版（参数→return/call_arg/field BFS 可达性）。先做 query-time summary 即可。CFG 是未来精度增强能力，不是 P5 MVP 的前置门槛；P5 优先完成 Level 3 facts，并在事实源统一后再接入 summary bridge。

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

## 5. Graph 分层加载

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

## 6. Engine / CLI / MCP 边界演进

当前 `atlas-engine` facade crate 已存在，并 re-export types、db、extraction、resolution、graph、analysis、search、context、filesync。这里的后续演进重点是稳定 public API 和交互契约，而不是重新拆分 workspace。

稳定时机：

```text
当前 atlas-engine facade + 内部 crates 已完成
  -> 稳定 variable provenance / caller path 语义精度和 public API
  -> 冻结 atlas-engine facade 的最小可用 API
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
- 不规划污点分析产品线；变量来源追踪和调用路径查询是分析主线。
- 项目级持久化和索引策略可以先保留在 Atlas 应用层，后续为 Atlas/Corpus 分支分别适配。

不要把 facade 存在等同于 API 已稳定；当前门槛不是“能跑通 E2E”，而是 trace 语义、capability 边界和测试断言已经足够稳定。

## 7. 后续分叉方向

Engine / CLI / MCP 边界拆清后，演进分为两条线。

### 7.1 Atlas 分支：单仓库、单版本索引

目标：

- 做好当前 Atlas 主线：本地单仓库、单版本、workspace graph。
- 稳定 `.atlas/atlas.db`、GraphSnapshot、search/context/MCP。
- 强化 incremental sync、影响面分析、调用图、变量来源追踪和调用路径查询。
- 面向日常代码理解、审查、Agent 上下文构建和路径分析。

该分支继续使用 project-relative path 作为文件身份基础。

### 7.2 Corpus 分支：大型项目多版本索引查询

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

## 8. 语言支持演进

当前代码已经接入两类非 MVP 语言：

- `all-languages` 包含的 post-MVP DataflowBasic frontends：Go、Rust、C#、PHP、Ruby、Kotlin。
- 显式 opt-in experimental frontends：Bash、Cangjie。

这些语言已经不是“未来可以引入”的空白项，但也还没有生产化到完整 path-level 变量来源追踪层。Swift 目前没有代码实现、feature flag 或 grammar 依赖。

当前 MVP 语言按能力等级推进：

```text
Level 0: parse/index only, trace unsupported
Level 1: symbols + references + calls
Level 2: bindings + simple assignments
Level 3: field access + call args + returns
Level 4: CFG (future precision layer, not P5 gate)
Level 5: lightweight interprocedural summaries
```

当前能力边界和演进策略：

| 语言 | 当前边界 | 下一步重点 | 用户交互约束 |
|---|---|---|---|
| TypeScript | `DataflowBasic` / Level 3 主目标，Level 5 未完成 | 真实源码 fixture、函数摘要、caller arg -> callee param bridge | 展示 `capability_level=dataflow_basic`；跨函数结果标注 summary/best-effort |
| JavaScript | `DataflowBasic` / Level 3 主目标，沿 TS JS grammar | 与 TS 共用 dataflow/query 但保持 language 独立 | 展示 `language=javascript`，不要混用 TS 名称 |
| Python | `DataflowBasic` / Level 3 主目标，动态语义低置信度 | import alias、attribute access、keyword args、returns fixture | 动态属性、反射调用、monkey patch 输出 limitation |
| Java | `DataflowBasic` best-effort，CFG/跨函数未完成 | method invocation、argument、return、package/import resolution、path smoke | caller/callee 可用；变量来源低置信度或 partial 时明确 diagnostics |
| C | include-aware `DataflowBasic` best-effort | callsite args、局部 assignment、return、include provenance、path smoke | 宏、函数指针、复杂指针别名标 low confidence/unsupported |
| C++ | include-aware `DataflowBasic` best-effort | method call、namespace、simple assignment、return、path smoke | 模板、重载、ADL 不得宣称精确 |
| ArkTS | TypeScript grammar fallback 的 `DataflowBasic` best-effort | ArkTS fixture、语言标识、TS fallback provenance | 输出必须说明 ArkTS via TS grammar fallback |
| Go | Post-MVP `DataflowBasic`，已在 `all-languages` | symbols/references/imports/calls golden fixture、dataflow smoke、package/import resolution | CFG/跨函数返回 unsupported；变量来源必须标注 limitation |
| Rust | Post-MVP `DataflowBasic`，已在 `all-languages` | macro/trait/impl 边界 fixture、dataflow smoke、call graph 置信度 | macro、lifetime、trait dispatch 不得宣称精确 |
| C# | Post-MVP `DataflowBasic`，已在 `all-languages` | namespace/partial class/delegate/event fixture、dataflow smoke | partial class 合并、delegate/event 语义保持 limitation |
| PHP | Post-MVP `DataflowBasic`，已在 `all-languages` | namespace/use alias、method call、closure fixture、dataflow smoke | 动态 method call、runtime include 标 unsupported/low confidence |
| Ruby | Post-MVP `DataflowBasic`，已在 `all-languages` | class/module/method/mixin fixture、dataflow smoke | method_missing、define_method、mixin 展开不宣称精确 |
| Kotlin | Post-MVP `DataflowBasic`，已在 `all-languages` | package/import、class/object、extension function fixture、dataflow smoke | companion/object/extension 函数 limitation 显式展示 |
| Bash | Experimental opt-in Symbolic | command/function/source fixture、低置信度 diagnostics | 默认/all-languages binary 不发现 `.sh/.bash`；命令调用标 low confidence |
| Cangjie | 不属于 MVP；experimental opt-in | grammar spike、minimal adapter、definitions/calls fixture | 默认/all-languages binary 不发现 `.cj/.cangjie`；启用后 trace 默认 unsupported，不返回静默空结果 |

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
- capability profile、CLI/MCP unsupported diagnostics 和测试规范同步更新。

## 9. Search 与 Context 演进

后续增强：

- 更强的 hybrid ranking：FTS、qualified name、graph centrality、path relevance、kind bonus。
- natural language query 到 symbol-like terms 的稳定抽取。
- import/export node 自动归约到 definition。
- context 输出加入 relationship map、confidence warnings、truncation note。
- test/non-production downrank 和 per-file diversity cap。

## 10. MCP 演进

当前已注册工具：

- `atlas_status`
- `atlas_files`
- `atlas_search`
- `atlas_symbol`
- `atlas_neighbors`
- `atlas_callers`
- `atlas_callees`
- `atlas_callgraph`
- `atlas_path`
- `atlas_explore`
- `atlas_impact`
- `atlas_context`
- `atlas_trace_point`
- `atlas_trace_variable`
- `atlas_trace_caller_path`
- `atlas_language_capabilities`
- `usages`
- `dependencies`
- `dependents`

V1 前必须处理：

- 文档化 MCP 命名策略：Atlas 专属工具使用 `atlas_` 前缀，通用语义工具 `usages`、`dependencies`、`dependents` 保持短名。
- 是否新增 `atlas_dataflow_path`，或继续以 `atlas_trace_variable` 作为 dataflow path 主入口。
- 为所有工具补齐 required schema、参数错误、路径限制、bounded output、confidence/provenance 测试。
- 明确 MCP graph snapshot 刷新策略。

所有工具必须保持：

- bounded output
- project path validation
- explicit confidence/provenance
- structured JSON plus Markdown context where useful

## 11. Schema 迁移（基础设施已完成）

schema 迁移基础设施已于 2026-05 落地：

- [x] `CURRENT_SCHEMA_VERSION` 与应用版本绑定。
- [x] 打开数据库时执行版本检查：若 DB 版本低于当前版本，运行有序迁移链（ALTER TABLE / CREATE INDEX 等）自动升级；若高于当前版本，拒绝打开并报告不兼容。
- [x] `atlas doctor` 区分"可自动迁移"（迁移链覆盖）与"需手动重建"（无迁移路径或 DB 来自更新版本）两种情况，并给出明确指引。
- [x] 迁移通过 `schema_versions` 表记录，支持幂等重放。

当前 `MIGRATIONS` 数组为空（V1），未来 schema 变更时在此添加迁移条目即可。

## 12. 不建议近期投入

- 完整编译器级 C/C++ 语义。
- 完整 Java classpath/build system 解析。
- Python 动态类型精确推断。
- 全语言 framework resolver 生态。
- 提前构建大型 graph database。
- 在 trace 语义精度和 public API 稳定前冻结/扩张 `atlas-engine` facade API。
- 在 engine/CLI/MCP 边界拆清前开启 Corpus 分支。
