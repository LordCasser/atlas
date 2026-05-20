# Atlas 测试规范

本文定义不同阶段的测试深度、准入门槛和禁止事项。目标是让测试能真实约束架构演进，而不是只证明某个局部 helper 可以工作。

## 1. 总体原则

1. 测试必须对应阶段目标。早期可以验证模型和算法雏形；进入能力验收时，必须验证真实入口和真实数据链路。
2. 单元测试可以使用手写 facts、mock data 和小型内存图；端到端测试不得用手写 facts 替代 extraction、storage、resolution、graph 或 analysis 主链路。
3. 默认 feature 和关键 feature 组合都必须可编译。`mcp`、`sync`、`all-languages` 不是文档能力，而是验收矩阵的一部分。
4. Golden fixture 用于锁定抽取输出，不用于证明产品能力已经端到端可用。
5. 新增语言、新增 schema、新增 CLI/MCP 工具、新增 analysis 能力，都必须同时补测试和文档里的阶段状态。
6. 低置信度、启发式、fallback 行为必须在测试里显式断言 confidence、strategy 或 diagnostics，不能只断言“有结果”。

## 2. 测试分层

### 2.1 单元测试

适用范围：

- ID 生成、enum roundtrip、range 工具函数。
- query parser、name matcher、scoring、rule matcher。
- 单个 builder 的局部行为，例如 CFG 节点生成、path tracer、taint worklist propagation。

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

- `atlas init/index/sync/search/status/files/context/taint/doctor`。

要求：

- 对用户可见能力，至少要有一个真实临时项目测试实际命令路径。
- CLI 输出可以断言稳定 JSON 或关键文本片段；不要依赖无意义的进度输出。
- `index` 必须验证单文件失败不会中断整个项目，并验证结构化失败报告。
- `taint` 必须验证真实源码项目经过 `index` 后能产生 finding 和 path steps。

### 2.5 MCP 测试

适用范围：

- MCP tool registration、schema、bounded output、错误输出、taint/query/context 工具。

要求：

- `cargo test --features "mcp"` 必须通过。
- 新增 MCP 工具必须测试注册名、required schema、正常调用和错误调用。
- 涉及项目文件或源码片段的工具必须测试 project path 限制。
- 输出必须断言 bounded 行为、confidence/provenance 暴露和结构化 JSON。

### 2.6 端到端测试

适用范围：

- 当前阶段声明已经可用的产品能力。

要求：

- 必须从真实源码开始，经过真实入口：discovery/index 或等价 pipeline。
- 必须经过 SQLite 持久化，而不是直接把 facts 传给分析器。
- 必须经过用户入口：CLI、MCP 或明确等价的 public API。
- 必须断言最终用户可消费结果，包括 ID、range、confidence、provenance、path steps 和 truncation/budget 信息。

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
- 旧 `RawEdge` dataflow 路径必须删除、隔离或明确标记为兼容遗留路径，不能作为 taint 输入。

### P4：CFG 基础

最低要求：

- 函数 Entry/Exit、Statement、Branch、Loop、Return、Throw、Join 的 builder 单元测试。
- 每种已支持 CFG 的语言至少有一个源码 fixture。

完成标准：

- CFG 节点必须绑定真实 function symbol。
- CFG edge 必须同属一个 function。
- 不支持的语言结构必须通过 diagnostics 或文档显式标记。

### P5：Taint MVP

最低要求：

- rule loader 覆盖默认规则、用户规则、覆盖策略、非法规则诊断。
- taint engine 单元测试可以使用手写 `DataNode` 和 `DataFlowEdge`。
- path tracer 单元测试可以使用手写图。

完成标准：

- 每种 MVP 语言至少有一个真实源码 fixture：source -> propagation -> sink。
- fixture 必须经过 extraction -> store -> dataflow_edges -> taint engine -> finding -> path steps。
- `atlas taint` 必须能在 fixture 项目上输出稳定结果。
- MCP 或等价 public API 必须能查询 finding/path，并返回 bounded、结构化输出。
- 测试必须断言 source range、sink range、rule id、severity、confidence、path step 顺序。
- sanitizer 和 max depth 必须有负向测试。
- 只用 canned facts 的测试只能证明 engine，不计入 P5 端到端验收。

### Engine / CLI / MCP 拆分阶段

最低要求：

- 拆分前后的 public API 行为保持一致。
- `atlas-engine` 不依赖 CLI 参数解析、MCP transport 或交互输出格式。
- CLI/MCP 只调用 engine/API，不复制 resolver、graph、analysis 算法。

完成标准：

- engine crate 有独立单元和集成测试。
- CLI crate 有命令级 smoke/E2E 测试。
- MCP crate 有 tool schema、routing、bounded output 测试。
- 原有 all-languages、mcp、sync 组合测试继续通过。

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
cargo test --features "all-languages"
cargo test --features "mcp"
cargo test --features "sync"
cargo test --features "all-languages,mcp,sync"
```

如果某个 feature 因外部依赖不可用而不能运行，必须在变更说明或 PR 中写清楚原因、影响范围和补偿验证。不能把 feature 编译失败视为“非默认路径所以可忽略”。

## 5. 禁止事项

1. 禁止用手写 facts 的测试宣称端到端能力完成。
2. 禁止只断言数量大于 0 来证明 resolution、graph、taint 正确。
3. 禁止 MCP/CLI 工具只注册不测试实际调用。
4. 禁止新增 schema 表但不测试 insert/query/delete/cascade。
5. 禁止修改 golden expected 而不说明语义原因。
6. 禁止在拆分 crate 前跳过当前阶段端到端门禁。

## 6. 当前项目的测试缺口

当前已知缺口应优先修复：

1. `mcp` feature 必须可编译，并纳入默认验证矩阵。
2. `atlas taint` 需要真实源码端到端 fixture，而不是只依赖 canned `DataNode` 测试。
3. `DataNode.function_id`、`callsite_args`、binding use scanning 需要补齐，否则 taint CLI 可能无法读取真实抽取结果。
4. `ParseWorkerPool` 需要接入 `atlas index` 和 `sync` 的生产路径。
5. 旧 `RawEdge` dataflow 路径需要清理或明确降级，避免和 `DataFlowEdge` 双轨腐化。
