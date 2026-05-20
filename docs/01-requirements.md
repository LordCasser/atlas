# Atlas 需求规格

Atlas 是一个 local-first Rust-native 代码语义图谱引擎。它扫描本地代码库，基于 tree-sitter 抽取符号、作用域、引用、调用、import/include、数据流和控制流事实，持久化到 `.atlas/atlas.db`，并通过 CLI 与 MCP 为 LLM Agent 提供搜索、调用分析、依赖分析、影响面分析、上下文构建和安全分析能力。

## 1. 产品定位

Atlas 的核心用户是：

- LLM Agent
- 代码审查和代码理解工具
- 调用图、依赖图、影响面分析工具
- 基于数据流的安全分析工具

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
- `atlas taint` where analysis feature is available

## 5. 非功能需求

- 性能：parallel parse、batch SQLite writes、read-mostly query snapshot、bounded caches。
- 安全：不上传代码；MCP 访问必须限制在 `projectPath` 内；读取源码片段必须校验路径。
- 可解释：semantic edge、resolution、taint finding 必须可追溯到引用位置或数据流路径。
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

当前阶段不先做 crate 拆分，也不先开启 Corpus 分支。当前阶段必须基于现有架构，把污点分析做到端到端可验证。

阶段完成条件：

1. MVP 语言均完成污点分析所需的基础抽取链路：symbols、references、callsites、bindings、data_nodes、dataflow_edges，CFG where applicable。
2. 每种 MVP 语言至少有一组 source -> propagation -> sink 的 taint fixture。
3. `atlas taint` 能对 fixture 项目输出 finding、severity、confidence、source/sink、path steps。
4. MCP 或等价工具能查询 taint finding/path，并返回 bounded、可解释输出。
5. 污点结果能回溯到源码 range、dataflow edge 和 rule。
6. 测试覆盖端到端路径，而不只覆盖类型和单个 builder。

只有完成上述 MVP 语言污点端到端测试后，才进入 crate 拆分阶段。
