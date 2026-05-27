# Atlas 测试规范

本文定义不同阶段的测试深度、准入门槛和禁止事项。目标是让测试能真实约束架构演进，而不是只证明某个局部 helper 可以工作。

## 1. 总体原则

1. 测试必须对应阶段目标。早期可以验证模型和算法雏形；进入能力验收时，必须验证真实入口和真实数据链路。
2. 单元测试可以使用手写 facts、mock data 和小型内存图；端到端测试不得用手写 facts 替代 extraction、storage、resolution、graph 或 analysis 主链路。
3. 默认 feature 和关键 feature 组合都必须可编译。`mcp`、`all-languages` 不是文档能力，而是验收矩阵的一部分；sync/filesync 当前是 engine 默认依赖，不是独立 Cargo feature。
4. Golden fixture 用于锁定抽取输出，不用于证明产品能力已经端到端可用。
5. 新增语言、新增 schema、新增 CLI/MCP 工具、新增 analysis 能力，都必须同时补测试和文档里的阶段状态。
6. 低置信度、启发式、fallback 行为必须在测试里显式断言 confidence、strategy 或 diagnostics，不能只断言“有结果”。

## 2. 测试分层

### 2.1 单元测试

适用范围：

- ID 生成、enum roundtrip、range 工具函数。
- query parser、name matcher、scoring、rule matcher。
- 单个 builder 的局部行为，例如 CFG 节点生成、path tracer、backward slicer。

要求：

- 可以使用内存数据和最小 fixture。
- 必须断言稳定 ID、边类型、置信度、错误分类等关键不变式。
- 不应把单元测试包装成“端到端”结论。

### 2.2 抽取 Golden 测试

适用范围：

- 每种语言的 definitions、references、imports/includes、scopes、callsites、bindings、data nodes、CFG 关键输出。
- query 文件和 adapter normalize 的兼容性。

要求：

- fixture 必须是源码文件，不是手写 facts。
- expected JSON 只保留可读、稳定、对架构有约束力的字段。
- 新增语言至少包含 `simple`、`imports/includes`、`calls`、`class/method` fixtures。
- 修改 golden expected 时，必须说明是修正旧错误、能力增强，还是语法覆盖变化。

### 2.3 集成测试

适用范围：

- 多文件 extraction -> store -> resolution -> GraphBuilder -> GraphSnapshot。
- import/export、include、path alias、container、callers/callees、search/context。

要求：

- 使用真实源码 fixture 或测试内写入的真实源码。
- 使用 `Store::open_in_memory()` 可以，但必须经过正常 `insert_file_facts` 和 resolver/builder。
- 必须断言关键结果的语义，例如 `main -> greet` 的 `Calls` edge，而不是只断言 edge 数量大于 0。
- 涉及 incremental sync 时，必须覆盖新增、修改、删除、旧 resolved target 失效、旧 edges 清理。

### 2.4 CLI 测试

适用范围：

- `atlas init/index/sync/search/status/files/context/trace/doctor`。

要求：

- 对用户可见能力，至少要有一个真实临时项目测试实际命令路径。
- CLI 输出可以断言稳定 JSON 或关键文本片段；不要依赖无意义的进度输出。
- `index` 必须验证单文件失败不会中断整个项目，并验证结构化失败报告。
- `trace` 必须验证真实源码项目经过 `index` 后能从指定位置产生变量来源和 caller path。
- `trace`、`status`、`doctor` 必须验证用户可见的语言能力边界输出，包括 capability level、supported/unsupported features、limitations 和 diagnostics。

### 2.5 MCP 测试

适用范围：

- MCP tool registration、schema、bounded output、错误输出、trace/query/context 工具。

要求：

- `cargo test -p atlas-cli --features "mcp"` 必须通过。
- 新增 MCP 工具必须测试注册名、required schema、正常调用和错误调用。
- 涉及项目文件或源码片段的工具必须测试 project path 限制。
- 输出必须断言 bounded 行为、confidence/provenance 暴露和结构化 JSON。
- trace/query/context 工具必须断言顶层 `capability` 对象存在，且不同语言的 unsupported/partial 结果不会被表示成无解释的空数组。

### 2.6 端到端测试

适用范围：

- 当前阶段声明已经可用的产品能力。

要求：

- 必须从真实源码开始，经过真实入口：discovery/index 或等价 pipeline。
- 必须经过 SQLite 持久化，而不是直接把 facts 传给分析器。
- 必须经过用户入口：CLI、MCP 或明确等价的 public API。
- 必须断言最终用户可消费结果，包括 ID、range、confidence、provenance、path steps 和 truncation/budget 信息。
- 必须覆盖至少一个“请求超出语言能力边界”的场景，断言 partial result、unsupported feature 和 limitation 对用户可见。

## 3. 阶段测试要求

### P0：语义绑定与 ID 修复

最低要求：

- `ReferenceId` 包含 `ReferenceKind` 的碰撞测试。
- `SemanticBinder` 填充 `source_symbol` 和 `scope_id` 的单元测试。
- extraction 后不存在 ghost source/caller 的 guard 测试。
- `Store::insert_file_facts` 的 defensive FK guard 测试。

完成标准：

- TypeScript、JavaScript、Python 至少覆盖 arrow function、class method、top-level call。
- 任何 adapter 不得再手写最终 `source_symbol` 或 `scope_id` 作为权威结果。

### P1：产品化索引基础

最低要求：

- `ParseWorkerPool` 覆盖 max file size、panic isolation、error category、IndexReport。
- discovery 覆盖 git-aware 和 filesystem fallback 的主要路径。
- `atlas index` 测试必须证明单文件失败不会中断项目。

完成标准：

- 生产 index 路径必须使用同一套 worker/report 机制，不能单独手写另一套错误分类。
- 大文件、坏文件、query error 的输出可被 CLI 和 MCP 消费。

### P2：Resolver 与 GraphBuilder 分离

最低要求：

- resolver 只更新 references resolved fields。
- GraphBuilder 从 resolved facts 创建 symbol_edges。
- sync 修改/删除文件时，旧 resolved facts 和旧 edges 被清理。

完成标准：

- 多文件 import/export/include 测试断言具体 edge kind、source、target、confidence、strategy。
- `GraphSnapshot` 只加载 symbol graph，不混入 dataflow/CFG。

### P3：Binding 与 DataFlow 基础

最低要求：

- 每种已声明支持 dataflow 的语言必须有 lexical binding fixture。
- `BindingDef`、`BindingUse`、`DataNode`、`DataFlowEdge` 的 ID 和 FK 测试。
- assignment、field load/store、call arg、return 至少各有一个源码级 fixture。

完成标准：

- 端到端链路中 `DataNode.function_id`、`binding_id`、`callsite_id` 在需要查询时必须可用。
- 不允许继续用 fake `SymbolId` 表示变量、表达式、参数或返回值数据流。
- 旧 `RawEdge` dataflow 路径必须删除或隔离，不能作为 trace 输入。

### P4：CFG 基础

最低要求：

- 函数 Entry/Exit、Statement、Branch、Loop、Return、Throw、Join 的 builder 单元测试。
- 每种已支持 CFG 的语言至少有一个源码 fixture。

完成标准：

- CFG 节点必须绑定真实 function symbol。
- CFG edge 必须同属一个 function。
- 不支持的语言结构必须通过 diagnostics 或文档显式标记。

### P5：变量来源追踪与调用路径查询 MVP

最低要求：

- 从 file/line/column 定位 ReferenceUse、BindingUse、DataNode，以及当前实现中的 callsite inline argument / call-arg DataNode。
- backward slicer 单元测试可以使用手写 `DataNode` 和 `DataFlowEdge`。
- caller path explorer 单元测试可以使用手写 symbol graph。
- path formatter 测试必须覆盖 bounded JSON 和 Markdown evidence。
- 不测试、不验收自动漏洞枚举、漏洞模式扫描、漏洞规则系统或 finding 产出。

完成标准：

- TypeScript/JavaScript/Python 至少各有一个真实源码 fixture：指定位置实参或变量 -> backward slice -> caller path。
- fixture 必须经过 extraction -> store -> resolution -> GraphBuilder -> dataflow/call graph -> trace query。
- CLI、MCP 或等价 public API 必须能查询 trace path，并返回 bounded、结构化输出。
- 测试必须断言每个 path step 的 kind、file、range、confidence/provenance 和截断行为。
- 全部 13 种 DataflowBasic 语言必须至少覆盖 symbols/references/imports/calls golden fixture 和 dataflow edge/path smoke 测试；各语言的具体能力边界通过 capability profile 和 unsupported/partial diagnostics 暴露，测试需覆盖 partial result 场景；Cangjie（Symbolic）只在显式启用对应 feature 的实验测试中覆盖。
- 每种 MVP 语言至少有一个 capability profile 快照测试；能力等级升级时必须同步更新 fixture 和用户可见输出断言。

### `atlas-engine` facade 稳定阶段

最低要求：

- `atlas-engine` facade public API 的行为保持一致，不能把 CLI 参数、MCP transport 或交互格式泄漏进 engine。
- `atlas-engine` 不依赖 CLI 参数解析、MCP transport 或交互输出格式。
- CLI/MCP 只调用 engine/API，不复制 resolver、graph、analysis 算法。

完成标准：

- engine crate 有独立单元和集成测试。
- CLI crate 有命令级 smoke/E2E 测试。
- MCP crate 有 tool schema、routing、bounded output 测试。
- 原有 default、all-languages、mcp 组合测试继续通过。

### Corpus 分支启动后

最低要求：

- Atlas 与 Corpus 的 ID、storage、query semantics 分开测试。
- Corpus 不复用 Atlas project-relative `FileId` 作为核心身份。

完成标准：

- Corpus 测试围绕 Git blob/tag/path/version mapping、跨版本查询、first-seen、diff/timeline。
- 共享 engine 的 parser/adapter/analysis 测试仍在 engine 层维护。

## 4. Feature 测试矩阵

每次合并前至少运行：

```bash
cargo test
cargo test -p atlas-cli --features "all-languages"
cargo test -p atlas-cli --features "mcp"
cargo test -p atlas-cli --features "all-languages,mcp"
```

如果某个 feature 因外部依赖不可用而不能运行，必须在变更说明或 PR 中写清楚原因、影响范围和补偿验证。不能把 feature 编译失败视为“非默认路径所以可忽略”。

## 5. 禁止事项

1. 禁止用手写 facts 的测试宣称端到端能力完成。
2. 禁止只断言数量大于 0 来证明 resolution、graph、trace 正确。
3. 禁止 MCP/CLI 工具只注册不测试实际调用。
4. 禁止新增 schema 表但不测试 insert/query/delete/cascade。
5. 禁止修改 golden expected 而不说明语义原因。
6. 禁止在稳定或扩张 `atlas-engine` public API 前跳过当前阶段端到端和语义精度门禁。

## 6. 当前项目的测试缺口

以下缺口按状态分列。已解决的标记为 ✅，待处理标记为 ⚠️。

### 已解决 ✅

1. **dataflow_edges TextRange 持久化** — `ts_dataflow_edges_complete_textrange` 和 `ts_dataflow_textrange_complete_roundtrip`（`integration.rs:645,598`）验证了 6 字段完整 byte/line/column 往返，注释明确标注了"原 bug 只存了 3/6 字段"的历史。
2. **Trace 端到端 fixture** — 四个测试文件共 135 个 trace 测试，覆盖跨文件调用（9+）、参数位置定位、unsupported/partial 结果（8+）、MCP 契约和 CLI 工作流。
3. **Go/Rust/C#/PHP/Ruby/Kotlin 能力快照与 dataflow smoke** — 每种语言 4 个 golden fixture（simple/imports/calls/class）；每种语言 2-3 个 dataflow smoke 测试，验证 DataNode 种类和 DataFlowEdge 产出。
4. **旧 RawEdge dataflow 路径移除** — `extract.rs:1082` 显式断言 `facts.raw_edges.is_empty()`；代码中 zero `RawEdge.*dataflow` 匹配项。RawEdge 仅用于 symbol_edges（Calls/Contains/Imports）。
5. **Cangjie capability 测试** — `test_cangjie_feature_matrix_no_call_graph` 断言 Cangjie call_graph 为 supported；`test_experimental_languages_in_all_compiled` 验证 feature flag gating 行为。
6. **Cangjie golden fixtures** — `golden_cangjie_{simple,imports,calls}` 三个 fixture 覆盖基本定义、引用、导入抽取（`golden.rs`，bootstrap 模式自动生成 expected.json）。
7. **DataNode cascade 删除** — `ts_delete_file_cascades_dataflow`（`integration.rs`）端到端验证：index → 确认 DataNode/DataFlowEdge 存在 → `delete_file_data` → 确认全部级联清理。SQLite `ON DELETE CASCADE` 外键约束保证一致性。
8. **Barrel re-export 链步行** — `ts_barrel_reexport_chain_resolves_to_source`（`integration.rs`）端到端验证：`main.ts` import barrel → barrel `export * from` → 原始定义文件，解析链路完整。`resolve_through_reexports` + `follow_reexport_chain` 支持递归链步行（最大深度 10，循环守卫），`resolve_relative_module` 处理 `./` 和 `../` 路径解析。
