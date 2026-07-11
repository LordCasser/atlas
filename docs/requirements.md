# Atlas 需求规格

Atlas 是一个 local-first Rust-native 代码语义图谱引擎。它扫描本地代码库，基于 tree-sitter 抽取符号、作用域、引用、调用、import/include、数据流和控制流事实，持久化到 `.atlas/atlas.db`，并通过 CLI 与 MCP 为 LLM Agent 提供搜索、调用分析、依赖分析、影响面分析、上下文构建、变量来源追踪和调用路径查询能力。

## 1. 产品定位

Atlas 的核心用户是：

- LLM Agent
- 代码审查和代码理解工具
- 调用图、依赖图、影响面分析工具
- AI 辅助路径分析工具

核心价值：

- 本地优先：代码和索引只保存在本机项目目录。
- 确定性：基于 tree-sitter AST/query，不用 AI 猜测生成图谱。
- 可解释：非结构语义关系必须携带置信度、来源和解析策略。
- 可增量：文件变化后只重建变更文件及受影响关系。
- MCP-first：Agent 消费是核心场景，不是 CLI 的附属功能。

## 2. 当前默认语言范围

默认构建固定支持以下 14 种语言：

| 语言 | 扩展名 | 策略 |
|---|---|---|
| TypeScript | `.ts`, `.tsx` | tree-sitter-typescript |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | tree-sitter-typescript 的 JS grammar |
| Python | `.py`, `.pyi`, `.pyx` | tree-sitter-python |
| Java | `.java` | tree-sitter-java |
| C | `.c`, `.h` | tree-sitter-c，头文件按启发式区分 C/C++ |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx` | tree-sitter-cpp |
| ArkTS | `.ets`, `.sts` | 复用 TypeScript grammar，但 language 存为 `arkts` |

Cangjie 已实现 **DataflowInterproc** 级别：基础定义/引用/导入、词法绑定、局部数据流、调用图和跨函数 summary 均已实现，CFG 暂未支持。现为默认编译语言之一。

当前代码已接入 Go、Rust、C#、PHP、Ruby、Kotlin 的 **DataflowInterproc** frontends。所有 14 种语言均为 DataflowInterproc 级别，具备完整 dataflow 抽取能力（参数、赋值、调用、字段访问、返回）、跨函数 summary 桥接（ArgToParam/ReturnToCall）和 e2e 测试。部分语言的 CFG 和特定跨函数路径仍有个别 gap（见各语言 capability profile limitations）。

## 3. 非目标

Atlas 不做：

- CodeGraph 的逐行 Rust rewrite。
- 兼容 `.codegraph` schema 或旧数据库。
- 23 种语言 feature parity。
- 完整编译器级类型检查。
- 完整 C/C++ preprocessing、模板实例化和重载解析。
- Python 动态类型精确推断。
- Java classpath/Maven/Gradle 完整解析。
- 完整 framework resolver 生态。
- 自动漏洞发现、自动漏洞枚举或完整 SAST 引擎。
- 污点分析引擎、taint rule engine 或内置漏洞规则系统。
- 基于漏洞模式自动扫描项目并产出 finding。
- 把大型多版本源码索引系统直接并入 Atlas 主体。

当前 best-effort 边界：

- C/C++ include-aware direct call graph。
- ArkTS via TypeScript grammar。
- Cangjie DataflowInterproc 抽取和调用图；CFG 暂未支持。
- Go/Rust/C#/PHP/Ruby/Kotlin 的 DataflowInterproc 抽取和调用图；具体 path-level 变量来源追踪、CFG 和跨函数 summary gap 以 capability limitations 和测试覆盖为准。
- 低置信度 name-based resolution。

## 4. 功能需求

### 文件发现

- 从 project root 扫描当前 14 种语言文件。
- git 项目优先使用 `git ls-files`，遵循 `.gitignore`。
- 非 git 项目回退 filesystem walk。
- 支持 include/exclude glob 和 `.atlasignore`。
- 默认排除 `.git`、`.atlas`、`node_modules`、`dist`、`build`、`out`、`target`、`__pycache__`、`.venv`、`venv`、`.gradle`、`.m2`。
- 单文件失败、超大文件、grammar panic 不得中断整个索引。

### 抽取

抽取架构必须是：

```text
tree-sitter queries + LanguageAdapter -> FileFacts
```

每个文件至少产出：

- file metadata
- symbols
- scopes
- references
- imports / includes / exports where available
- raw structural facts
- callsites
- bindings and binding uses where implemented
- data nodes and dataflow edges where implemented
- CFG nodes and CFG edges where implemented
- diagnostics

分析等级必须有稳定的用户可见语义：

- `Manifest`：只产出顶层声明符号，用于快速初始索引和 Focus materialize 候选；不得写入函数体内部局部定义。
- `ResolutionSymbols`：只作为 dependency / Focus dependency 物化目标层，不对用户宣称完整 structural。
- `Structural`：产出 symbols/scopes/references/callsites/call graph，可支持结构性搜索、context、callers/callees。
- `LazyDataflow`：L2 机制处方——按需为查询窗口产出 dataflow/CFG facts（Focus materialize 内部）；budget/pending 映射为 MCP `analysis.retry_after_ms` 或终态 `gaps`。
- `Full`：产出 structural + dataflow + CFG（语言支持时）+ persistent summaries；summary capability 只能在 summary tables 成功构建后对用户可见。

索引重跑必须按请求的 analysis mode 判断是否已满足目标能力，不能只比较文件 hash：

- `Manifest` run：hash 未变但缺 fresh complete manifest capability 的文件仍需重抽。
- `Structural` run：hash 未变但缺 fresh complete structural capability 的文件仍需重抽；已有 dataflow/full capability 可满足 structural。
- `Full` run：hash 未变但缺 fresh complete dataflow capability 的文件仍需重抽；summary capability 只能在 summary build 成功后出现。
- 文件只有在 `files.content_hash` 与当前磁盘 hash 一致，且 file-level `extraction_state` 的 complete capability 覆盖请求 mode 时，才可视为 clean。
- 缺失 `last_index_time`、`last_sync_time` 等可选 metadata 是正常状态，不得产生 warning；真正的查询错误才应报警。
- 当前开发线不为旧 DB schema 增加运行时兼容 fallback；schema、DDL 和读写代码必须保持同步，schema 变化后要求重建索引。

### 符号与引用

Structural/Full 模式按能力至少抽取：

- file/module/package/namespace
- class/struct/interface
- function/method/constructor
- field/property
- reliable variable/constant
- enum/enum member/type alias where grammar supports
- import/include/export declarations

引用必须保留 occurrence，不得只保存最终 edge。引用类型至少包括 calls、instantiates、references、imports/includes、extends、implements、decorates、type/return refs where feasible。

### Resolution

目标 Resolution pipeline 顺序如下。当前已落地实现状态以
[`architecture.md`](./architecture.md) 为准；未接入主路径的
resolver 组件不得在用户文档中描述为已完成能力。

1. builtin/external filter
2. scope-local exact lookup
3. container/class-local lookup
4. same-file exact lookup
5. import/include/package resolver
6. language-specific module resolver
7. same namespace/package lookup
8. framework hook optional
9. project-wide exact + proximity scoring
10. bounded fuzzy fallback

Resolution 结果必须写回引用事实，并包含 target、confidence、strategy/resolved_by、provenance/diagnostics。

### 图查询和上下文

必须支持：

- neighbors
- callers / callees
- callgraph
- impact
- shortest path
- usages / references
- file dependencies / dependents
- context / explore

图查询优先使用 `GraphSnapshot` 或按需加载的专用图结构，避免每一步访问 SQLite。

### 变量来源追踪与调用路径查询

当前分析主线不是全项目自动漏洞扫描，也不是污点分析。用户或 AI 可以把外部发现的疑似问题点、代码模式或具体变量作为查询入口；Atlas 只把它们当作普通代码位置和程序事实处理，返回变量来源、调用者链路和相关源码证据。Atlas 不判断“这是不是漏洞”，也不主动枚举项目里的漏洞模式。

必须支持的查询目标：

- 某个函数被哪些函数直接或间接调用。
- 某个调用点的某个实参来自哪里。
- 某个函数内的某个变量或表达式来自哪里。
- 某个目标变量是否来自函数参数、字段访问、返回值、import alias 或上游 caller 实参。
- 从指定位置向上游回溯数据来源和调用路径，并返回相关代码片段、range、confidence 和 provenance。

目标核心能力是 backward slice / provenance trace。当前实现以 local dataflow
和 caller path 为主，跨函数参数/返回传播只有在对应 facts、summary 和测试存在时才能宣称支持：

```text
target argument / variable
  -> local assignment source
  -> field/access path source
  -> function return source
  -> callee parameter
  -> caller argument
  -> caller chain / entry candidate
```

Atlas 不做 taint rule / finding 产品能力。Atlas 不包含 taint 代码、taint schema、taint rule/finding 产品能力——这些已从源码和 schema 中完全删除。不进入当前需求、路线图或验收门槛。当前阶段不要求、也不规划漏洞规则三元组、自动端到端漏洞传播扫描、全项目 finding 或内置漏洞规则生态。

解析侧需要提供的基础 facts 分为“当前 trace 主路径事实”和“后续恢复/增强事实”：

- `BindingDef` / `BindingUse`：区分定义和使用，记录作用域、range、shadowing 关系。
- `Callsite`：记录 callee、receiver、callee range、call range，并保存当前实现使用的 inline argument facts。
- `DataNode`：覆盖参数、局部变量、字面量、字段访问、调用结果、返回值、表达式和 import alias。
- `DataFlowEdge`：覆盖简单赋值、字段读取/写入、实参到形参、返回值到调用结果、变量到返回值等关系。
- `CallsiteArg`：已移除。`callsites.args_json` + call-arg `DataNode` 为当前唯一调用实参事实源；如未来需结构化实参表，应在 schema 中新增替代设计并同步测试。
- `FunctionSummary`：已实现持久化摘要层（Schema V2）：`function_summaries`、`summary_param_reaches`、`summary_return_sources`、`summary_call_arg_sources` 四张表，通过 `CrossFunctionBridge` 实现 ArgToParam 和 ReturnToCall 跨函数桥接。当前开发线不兼容旧 schema；schema 变化后必须重建索引。

语言能力按等级验收，不要求所有语言一次性达到同等精度：

```text
Level 0: parse/index only, trace unsupported
Level 1: symbols + references + calls
Level 2: bindings + simple assignments
Level 3: field access + call args + returns
Level 4: CFG (未来精度增强层，不作为当前 trace MVP 前置门槛)
Level 5: lightweight interprocedural summaries
```

当前语言能力边界必须以用户可见方式呈现：

所有 14 种语言均为 **DataflowInterproc** 级别。以下为各语言关键能力差异：

| 语言 | 当前 trace 边界 | 用户交互展示要求 |
|---|---|---|
| TypeScript | DataflowInterproc: 变量来源、call args、field access、return、CFG、跨函数 ArgToParam+ReturnToCall | 展示完整证据链；跨函数结果标注 depth、summary/heuristic 和 confidence |
| JavaScript | 与 TypeScript 共用 JS grammar 路径，DataflowInterproc | 展示同 TypeScript，但必须标注 `javascript`，不能混写成 `typescript` |
| Python | DataflowInterproc: scope-chain-aware binding, CFG, ArgToParam+ReturnToCall，confidence 0.72 | 对动态调用、属性链、import alias fallback 输出 lower confidence 或 unsupported diagnostics |
| Java | DataflowInterproc: ArgToParam+ReturnToCall, CFG，confidence 0.75 | 调用路径精确；参数、返回值、字段来源带 limitation/confidence |
| C | DataflowInterproc: ArgToParam+ReturnToCall, CFG，confidence 0.73；函数指针 limited depth 3 | 调用路径可低置信度展示；宏展开、函数指针、复杂指针别名显示 limitation |
| C++ | DataflowInterproc: ArgToParam+ReturnToCall, CFG，confidence 0.70；模板/重载/ADL 不建模 | 调用路径和局部来源必须标注 best-effort |
| ArkTS | DataflowInterproc via TS grammar，confidence 0.60；CFG WithLimitations(0.55)；named function/method branch-loop 已验证，ArkUI trailing-block/callback CFG 未建模 | 显示 `arkts via TypeScript grammar` provenance 与具体 limitation |
| Go | DataflowInterproc: ArgToParam+ReturnToCall, CFG，confidence 0.78 | 调用路径精确 |
| C# | DataflowInterproc: ArgToParam+ReturnToCall，CFG，confidence 0.72 | `using_statement` 和 branch/loop CFG；partial classes limitation |
| Rust | DataflowInterproc: ArgToParam+ReturnToCall，CFG，confidence 0.70 | 宏与 borrow 语义不建模 |
| PHP | DataflowInterproc: ArgToParam+ReturnToCall，confidence 0.62；CFG 未实现 | name-based binding 与动态调用 limitation |
| Ruby | DataflowInterproc: ArgToParam+ReturnToCall，CFG，confidence 0.65 | block/yield 为 best-effort |
| Kotlin | DataflowInterproc: ArgToParam+ReturnToCall，CFG，confidence 0.67 | extension receiver binding limitation |
| Cangjie | DataflowInterproc: ArgToParam+ReturnToCall，CFG，confidence 0.65 | postfixExpression/callSuffix limitation |

承载语言能力的输出（`atlas doctor`、trace envelope、相关 MCP 分析响应）必须从 `LanguageCapabilityProfile` 读取事实，不得在展示层重建能力表。Trace 内层冻结契约包含：

- `language`
- `capability_level`
- `supported_features`
- `unsupported_features`
- `limitations`
- `partial_result`

非 trace MCP 外层不复用 `partial_result`；它通过 `analysis.basis`、可选 `analysis.retry_after_ms`、可选 `coverage_counts` 和终态 `gaps` 表达覆盖与缺口。

当查询能力超出当前语言边界时，Atlas 必须返回结构化 diagnostics，例如 `unsupported_feature`、`best_effort_only`、`missing_fact`、`low_confidence_resolution`。用户交互中禁止只返回空路径而不解释原因。

### MCP

MCP 使用 JSON-RPC over stdio。当前公开工具使用无 `atlas_` 前缀的短名：

- project lifecycle: `project`
- symbol/search: `search`, `symbol`
- graph: `calls`, `impact`, `path`, `explore`
- trace: `trace`
- file dependencies: `file_dependencies`
- semantic analysis: `lifecycle`, `branch_diff`, `domain_rules`
- focus/lazy state: `tasks`, `resume_query`
- FP dispatch annotations: `fp_dispatches`

MCP 入口必须先 `project(action="open")` 同步激活项目；open 不做全项目扫描或索引。`search(scope=...)` 和其他 scoped 查询负责触发 focus/lazy materialization。MCP 不再暴露 `index`、`task_status`、`wait_for_task`、`resume_task` 或 `background=true` 参数；显式全项目索引只能通过 CLI `atlas index` 执行。

工具输出必须 bounded、结构化，并在涉及启发式关系时暴露 confidence/provenance。

触发 lazy structural、lazy dataflow 或 lazy CFG 的 MCP 工具必须返回可解释的能力状态：

- `analysis`：说明 scope、basis、summary；仅在 live tracker 仍有工作时提供 `retry_after_ms`。
- `gaps`：终态已知缺口，稳定映射为 `{scope, reason, detail}`。
- `query_id` + `tasks` + `resume_query`：让可恢复 refinement 可观测并最终收敛。
- 空结果或错误不得吞掉仍可恢复的 query 状态；不可恢复错误不得伪造 retry。
- CFG/semantic 工具已经基于 CFG 产出结果时，不得同时声明 CFG 不可用。
- `lifecycle` 必须在查询时从目标函数 CFG + dataflow 组合 semantic effects；不能因持久化
  `cfg_nodes.semantic_effects` 为空而返回零迁移。C/C++ local resource 和 field path 均是合法
  跟踪目标，默认 ownership matcher 必须覆盖项目已声明支持的内核 alloc/free 惯用法。
- `lifecycle` / `branch_diff` / `impact(semantic=true)` 的 MCP 实现必须经 `AnalysisRuntime` 编排（能力门控、CFG/dataflow
  ensure 与 I/O、effect composition、引擎调用）；handler 不得直调 analysis engine 或在
  handler 内完成上述编排。C/C++ 持久化 `alloc_fn` / `free_fn` / `cleanup_fn` 必须与默认
  resource config 合并后注入 effect composition，而不是仅加载后旁路或替换默认 matcher；
  非 C/C++ 的 `lifecycle` 以 `unsupported_language` gap 终态说明边界。
- `calls(direction=incoming|outgoing)` 固定 1-hop；`depth` 存在时必须给出未采纳警告；多跳走
  `direction=both` 或 callgraph。节点 `signature` 来自 store，不污染 graph NodeSummary。
- Focus 写库与 CLI 全量索引互斥：其他 live 进程持 `FileLock` 时 Focus/MCP 写路径立即 reject
  （`cli_index_lock_held`），不得 wait/queue。

Focus closure 必须满足以下正确性约束：

- symbol 查询以精确符号为扩展前沿；同文件其他 peer 不因共址而自动进入 call/type 闭包。
- import/include 文件优先只构建 `ResolutionSymbols`；只有被查询关系证明相关的文件才升级
  为 `Structural`。
- 请求深度驱动 bounded fixed point，并受公开深度上限、文件预算和时间预算约束。
- 图查询前台只同步保证精确 seed；多跳 call/type 扩展必须进入可追踪后台 closure，首个
  响应通过 retry/gaps 如实声明边界。函数内 semantic 查询不得为获取 CFG 而启动图扩展。
- coverage 只统计实际 built/cached facts。抽取失败、取消和预算耗尽必须成为终态 gap，
  不能伪装为完整，也不能永久 pending。
- 后台物化 facts 必须在 `resume_query` 重放前进入 graph snapshot；持久化成功但查询不可见
  不算完成。
- 冷 C/C++ type 查询必须在首次可消费结果中返回完整定义范围。
- Content hash 相同不能覆盖可证明的 structural 语义失效。旧 C/C++ 多行类型的一行范围
  和非 callable call owner 必须触发定向自愈重抽，无需全项目重索引。

### CLI

核心命令：

- `atlas index` (auto-init schema) / `atlas sync` (incremental)
- `atlas status` / `atlas doctor` / `atlas files`
- `atlas mcp` (MCP server, 15 open-first focus tools)
- `atlas` (no subcommand: from the project root, create/recover a usable DB and run the default structural index first if no basic-or-better index exists, then launch the interactive TUI)

CLI 参数必须失败得明确。`--analysis` 只允许 `manifest`、`structural`、`full`；未知值必须返回错误，不能静默降级为 Structural。

#### 裸 `atlas` TUI 首跑 UX

裸 `atlas` 是 TUI 入口，不接受 `--project`，使用当前工作目录作为 project root。
首跑行为必须满足：

- 已提前跑过 index，且持久化状态显示所有已索引文件至少有完整 `manifest` 层时，直接进入 TUI。
- 已有 `structural` 或 `full` index 时同样直接进入 TUI，不得重复默认索引。
- 完全没有 `.atlas/atlas.db`、数据库为空、数据库无法打开，或 schema 初始化失败时，先创建/恢复可用 DB。
- 损坏 DB 不直接覆盖；应保留为 `.corrupt.<timestamp>` 备份后再创建新 DB。
- 恢复出可用 DB 后，先运行与命令行 `atlas index` 默认值一致的 `structural` index。
- 默认 index 完成后才启动 TUI 交互界面。
- TUI 边缘/状态栏必须明确显示当前 index mode：`empty`、`manifest`、`structural`、`full` 或 `partial`。

## 5. 非功能需求

- 性能：parallel parse、batch SQLite writes、read-mostly query snapshot、bounded caches。
- 安全：不上传代码；MCP 访问必须限制在 `projectPath` 内；读取源码片段必须校验路径。
- 可解释：semantic edge、resolution、trace result 必须可追溯到引用位置、数据流路径或调用路径。
- 可测试：每种当前语言至少有 definitions、imports/includes、direct calls、class/method calls、inheritance/implements fixtures，并按 capability 覆盖 dataflow/CFG。
- 可扩展：新增语言主要新增 adapter、query、fixture 和必要 resolution rules，不修改中心 mega-extractor。

## 6. 验收标准

当前基线验收标准：

1. 全部 14 种语言能进入解析路径，均达到 DataflowInterproc 级别；Cangjie 已提升至 DataflowInterproc。
2. `atlas index` 能生成 `.atlas/atlas.db`（Schema V2）。
3. TUI / MCP `search` 工具能检索符号。
4. CLI 或 MCP 能查询基本 callers/callees。
5. 所有语言 import/include resolution 可用。
6. C/C++ include-aware best-effort resolution 可用。
7. GraphSnapshot 支撑低延迟图查询。
8. MCP 输出可被 Agent 消费，并控制预算。
9. 关系结果暴露 confidence/provenance。
10. 语言 fixtures 和集成测试覆盖主链路。
11. 持久化跨函数摘要层（Schema V2）已实现。
12. MCP/shared pipeline、CLI index、CLI sync、以及裸 `atlas` 首跑 structural index 在各自声明的分析等级下语义一致；删除文件、Full summaries、lazy diagnostics、capability mask 和 TUI index-mode 状态栏都有发布前验证。
13. 部分索引的大型项目中，首次 cold symbol/explore 能按符号级 bounded closure 收敛；
    dependency-only 文件不被误算 structural，后台成功/失败都能到达可解释终态。
14. TUI 原生 search 不等待或同步构建全量 graph snapshot；首次 graph-backed detail 和
    stale refresh 在可取消后台 job 中完成，UI 线程持续处理 tick、渲染和退出。

## 7. 当前阶段验收焦点

当前 workspace crate 拆分和 15-tool MCP 收敛已经完成。当前阶段不再做新的大拆分，也不启动 Corpus 产品线；重点是稳定 Atlas 1.5.x 的公共契约、文档、发布验证和真实项目性能。

阶段完成条件：

1. 所有 14 种 DataflowInterproc 语言维持 trace 所需 facts 与 ArgToParam/ReturnToCall fixture；CFG 以 capability profile 为准（当前仅 PHP 不支持）。
2. TypeScript/JavaScript/Python 至少有真实源码 fixture 覆盖"指定位置 → 变量来源 → caller path"。
3. 所有语言维持 DataflowInterproc 边界；具体 gap 通过 capability profile、golden fixture 和端到端断言文档化，不用 ignored/should-panic 测试伪装已支持能力。
4. MCP 或 high-level `Engine` 能按 file/line/column 查询 trace point / backward trace，并能按 symbol selector 查询 caller path；CLI 当前不提供 trace 子命令。
5. 输出包含 path steps、源码 range、相关代码片段或 evidence、confidence/provenance、截断说明。
6. 测试覆盖真实 extraction -> store -> resolution -> dataflow/call graph -> trace 查询链路，并对 `ArgToParam`、`ReturnToCall`、evidence、终态 retry/gaps 等具体语义做断言。

只有当前 trace 精度、capability 边界和测试断言稳定后，才冻结/扩张可复用 `atlas-engine` facade API。Corpus 分支仍必须等 engine/API 边界稳定后再启动。
