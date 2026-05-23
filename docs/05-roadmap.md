# 未来架构演进

本文只记录未完全落地或仍需增强的设计方向。当前实现状态见 [当前架构实现](./03-current-architecture.md)。

## 1. 近期优先级

1. 在当前 12-crate workspace 内继续稳定变量来源追踪和调用路径查询所需 facts：bindings、binding uses、callsites、inline call arguments、data nodes、dataflow edges、call graph；`callsite_args` 表已移除，调用实参统一使用 `callsites.args_json` + call-arg DataNode；FunctionSummary 已实现 query-time 基础版。
2. TypeScript/JavaScript/Python 已有 backward slice 端到端测试，下一步重点是把启发式 dataflow 转成更受 AST 结构约束的事实，并收紧断言。
3. Java/C/C++/ArkTS 以及 Go/Rust/C#/PHP/Ruby/Kotlin 当前以 Symbolic best-effort 和 explicit unsupported diagnostics 为边界；不得把未覆盖的 Level 3 来源追踪描述为已完成能力。Bash/Cangjie 只在显式启用时展示 experimental capability。
4. 继续打磨 trace CLI/MCP 输出，使 capability、partial result、diagnostics、confidence/provenance 对 Agent 足够明确。
5. 不再做 workspace 大拆分；后续只在 trace 精度和 public API 稳定后，考虑抽出可复用 `atlas-engine` crate。Corpus 仍不启动。

## 2. P5：变量来源追踪与调用路径查询 MVP

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
- Java/C/C++/ArkTS 至少提供 Level 1 callers/callees fixture，并为 Level 2/3 能力提供 best-effort fixture 或显式 unsupported diagnostics；Cangjie 不进入 MVP fixture 门槛。
- CLI/MCP 或等价接口必须能读取 trace path，并提供 bounded 输出。
- 测试必须断言每一步的 kind、range、file、confidence/provenance。
- 失败或不支持的语言能力必须显式标记，不得静默通过。

## 3. 函数摘要与轻量跨函数追踪

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

## 5. Engine / CLI / MCP 边界演进

拆分时机：

```text
当前 12-crate workspace 已完成
  -> 稳定 variable provenance / caller path 语义精度和 public API
  -> 抽出可复用 atlas-engine crate
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

不要过早抽出 `atlas-engine`；当前门槛不是“能跑通 E2E”，而是 trace 语义、capability 边界和测试断言已经足够稳定。

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

当前代码已经接入两类非 MVP 语言：

- `all-languages` 包含的 post-MVP Symbolic frontends：Go、Rust、C#、PHP、Ruby、Kotlin。
- 显式 opt-in experimental frontends：Bash、Cangjie。

这些语言已经不是“未来可以引入”的空白项，但也还没有生产化到变量来源追踪层。Swift 目前没有代码实现、feature flag 或 grammar 依赖。

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
| Java | Level 1 当前保底，Level 2/3 待 fixture 约束 | method invocation、argument、return、package/import resolution | caller/callee 可用；变量来源不可用时明确 unsupported |
| C | include-aware Level 1/2 best-effort | callsite args、局部 assignment、return、include provenance | 宏、函数指针、复杂指针别名标 low confidence/unsupported |
| C++ | include-aware Level 1/2 best-effort | method call、namespace、simple assignment、return | 模板、重载、ADL 不得宣称精确 |
| ArkTS | TypeScript grammar fallback 的 Level 1/2 best-effort | ArkTS fixture、语言标识、TS fallback provenance | 输出必须说明 ArkTS via TS grammar fallback |
| Go | Post-MVP Symbolic，已在 `all-languages` | symbols/references/imports/calls golden fixture、package/import resolution | 变量来源、CFG、use-def 返回 unsupported diagnostics |
| Rust | Post-MVP Symbolic，已在 `all-languages` | macro/trait/impl 边界 fixture、call graph 置信度 | macro、lifetime、trait dispatch 不得宣称精确 |
| C# | Post-MVP Symbolic，已在 `all-languages` | namespace/partial class/delegate/event fixture | partial class 合并、delegate/event 语义保持 limitation |
| PHP | Post-MVP Symbolic，已在 `all-languages` | namespace/use alias、method call、closure fixture | 动态 method call、runtime include 标 unsupported/low confidence |
| Ruby | Post-MVP Symbolic，已在 `all-languages` | class/module/method/mixin fixture | method_missing、define_method、mixin 展开不宣称精确 |
| Kotlin | Post-MVP Symbolic，已在 `all-languages` | package/import、class/object、extension function fixture | companion/object/extension 函数 limitation 显式展示 |
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

## 10. Schema 迁移（已完成）

schema 迁移基础设施已于 2026-05 落地：

- [x] `CURRENT_SCHEMA_VERSION` 与应用版本绑定。
- [x] 打开数据库时执行版本检查：若 DB 版本低于当前版本，运行有序迁移链（ALTER TABLE / CREATE INDEX 等）自动升级；若高于当前版本，拒绝打开并报告不兼容。
- [x] `atlas doctor` 区分"可自动迁移"（迁移链覆盖）与"需手动重建"（无迁移路径或 DB 来自更新版本）两种情况，并给出明确指引。
- [x] 迁移通过 `schema_versions` 表记录，支持幂等重放。

当前 `MIGRATIONS` 数组为空（V1），未来 schema 变更时在此添加迁移条目即可。

## 11. 不建议近期投入

- 完整编译器级 C/C++ 语义。
- 完整 Java classpath/build system 解析。
- Python 动态类型精确推断。
- 全语言 framework resolver 生态。
- 提前构建大型 graph database。
- 在 trace 语义精度和 public API 稳定前抽出 `atlas-engine`。
- 在 engine/CLI/MCP 边界拆清前开启 Corpus 分支。
