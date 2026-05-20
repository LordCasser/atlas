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

## 2. MVP 语言范围

MVP 固定支持：

| 语言 | 扩展名 | 策略 |
|---|---|---|
| TypeScript | `.ts`, `.tsx` | tree-sitter-typescript |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | tree-sitter-typescript 的 JS grammar |
| Python | `.py`, `.pyi`, `.pyx` | tree-sitter-python |
| Java | `.java` | tree-sitter-java |
| C | `.c`, `.h` | tree-sitter-c，头文件按启发式区分 C/C++ |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx` | tree-sitter-cpp |
| ArkTS | `.ets`, `.sts` | MVP 复用 TypeScript grammar，但 language 存为 `arkts` |
| Cangjie | `.cj`, `.cangjie` | 先 grammar spike，再 minimal adapter |

非 MVP 语言可以作为 opt-in/future features，但不纳入当前验收。

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
- 污点分析引擎、taint rule engine、source/sink/sanitizer 规则系统。
- 基于漏洞模式自动扫描项目并产出 finding。
- 把大型多版本源码索引系统直接并入 Atlas 主体。

MVP 可以 best-effort：

- C/C++ include-aware direct call graph。
- ArkTS via TypeScript grammar。
- Cangjie grammar-based minimal extraction。
- 低置信度 name-based resolution。

## 4. 功能需求

### 文件发现

- 从 project root 扫描 MVP 语言文件。
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

### 符号与引用

MVP 至少抽取：

- file/module/package/namespace
- class/struct/interface
- function/method/constructor
- field/property
- reliable variable/constant
- enum/enum member/type alias where grammar supports
- import/include/export declarations

引用必须保留 occurrence，不得只保存最终 edge。引用类型至少包括 calls、instantiates、references、imports/includes、extends、implements、decorates、type/return refs where feasible。

### Resolution

Resolution pipeline 顺序：

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

### 变量来源与调用路径查询

当前分析主线不是全项目自动漏洞扫描，也不是污点分析。用户可以指定某个函数、变量、调用点、文件行列或代码模式；Atlas 只提供结构化证据，让 AI 或用户继续判断这些路径是否和某个漏洞相关。

必须支持的查询目标：

- 某个函数被哪些函数直接或间接调用。
- 某个调用点的某个实参来自哪里。
- 某个函数内的某个变量或表达式来自哪里。
- 某个问题变量是否来自函数参数、字段访问、返回值、import alias 或上游 caller 实参。
- 从指定位置向上游回溯数据来源和调用路径，并返回相关代码片段、range、confidence 和 provenance。

核心能力是 backward slice / provenance trace：

```text
target argument / variable
  -> local assignment source
  -> field/access path source
  -> function return source
  -> callee parameter
  -> caller argument
  -> caller chain / entry candidate
```

Atlas 不做 taint rule / finding 产品能力。已有 taint 相关代码和 schema 只视为历史原型，不进入当前需求、路线图或验收门槛。当前阶段不要求、也不规划 source/sink/sanitizer 规则、自动 source-to-sink 扫描、全项目 finding 或内置漏洞规则生态。

语言能力按等级验收，不要求所有语言一次性达到同等精度：

```text
Level 1: symbols + references + calls
Level 2: bindings + simple assignments
Level 3: field access + call args + returns
Level 4: CFG
Level 5: lightweight interprocedural summaries
```

当前 trace 主线至少要求 TypeScript/JavaScript/Python 达到 Level 3，并逐步补 Level 5 的轻量摘要。Java/C/C++/ArkTS/Cangjie 可以先提供 Level 1/2 best-effort，但输出必须标注能力和置信度。

### MCP

MCP 使用 JSON-RPC over stdio。核心工具：

- `atlas_status`
- `atlas_files`
- `atlas_search`
- `atlas_symbol`
- `atlas_neighbors`
- `atlas_callers`
- `atlas_callees`
- `atlas_callgraph`
- `atlas_impact`
- `atlas_path`
- `atlas_context`
- `atlas_explore`
- future/analysis tools: `atlas_trace_variable`, `atlas_trace_point`, `atlas_dataflow_path` where implemented

工具输出必须 bounded、结构化，并在涉及启发式关系时暴露 confidence/provenance。

### CLI

核心命令：

- `atlas init`
- `atlas index`
- `atlas sync`
- `atlas search`
- `atlas status`
- `atlas files`
- `atlas context`
- `atlas mcp`
- `atlas doctor`
- `atlas trace` where analysis feature is available

## 5. 非功能需求

- 性能：parallel parse、batch SQLite writes、read-mostly query snapshot、bounded caches。
- 安全：不上传代码；MCP 访问必须限制在 `projectPath` 内；读取源码片段必须校验路径。
- 可解释：semantic edge、resolution、trace result 必须可追溯到引用位置、数据流路径或调用路径。
- 可测试：每种 MVP 语言至少有 definitions、imports/includes、direct calls、class/method calls、inheritance/implements fixtures。
- 可扩展：新增语言主要新增 adapter、query、fixture 和必要 resolution rules，不修改中心 mega-extractor。

## 6. 验收标准

MVP 完成标准：

1. 8 种 MVP 语言能进入解析路径；Cangjie 至少完成 grammar spike 和 minimal adapter。
2. `atlas index` 能生成 `.atlas/atlas.db`。
3. `atlas search` 能检索符号。
4. CLI 或 MCP 能查询基本 callers/callees。
5. TS/JS/ArkTS/Python/Java import resolution 可用。
6. C/C++ include-aware best-effort resolution 可用。
7. GraphSnapshot 支撑低延迟图查询。
8. MCP 输出可被 Agent 消费，并控制预算。
9. 关系结果暴露 confidence/provenance。
10. 语言 fixtures 和集成测试覆盖主链路。

## 7. 当前阶段验收焦点

当前阶段不先做 crate 拆分，也不先开启 Corpus 分支。当前阶段必须基于现有架构，把变量来源追踪和调用路径查询做到端到端可验证。

阶段完成条件：

1. MVP 语言按能力等级补齐 trace 所需 facts：symbols、references、callsites、bindings、data_nodes、dataflow_edges，CFG where applicable。
2. TypeScript/JavaScript/Python 至少有真实源码 fixture 覆盖“指定位置 -> 变量来源 -> caller path”。
3. Java/C/C++/ArkTS/Cangjie 至少能提供 Level 1 调用图和 Level 2/3 的 best-effort 局部来源追踪，不能支持的能力必须显式标记。
4. CLI、MCP 或等价 public API 能按 file/line、function+variable、callsite+argument 查询 backward trace。
5. 输出包含 path steps、源码 range、相关代码片段、confidence/provenance、截断说明。
6. 测试覆盖真实 extraction -> store -> resolution -> dataflow/call graph -> trace 查询链路，而不只覆盖类型和单个 builder。

只有完成上述变量来源追踪和调用路径查询端到端能力后，才进入 crate 拆分阶段。
