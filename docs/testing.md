# Atlas 测试规范

本文定义不同阶段的测试深度、准入门槛和禁止事项。目标是让测试能真实约束架构演进，而不是只证明某个局部 helper 可以工作。

## 1. 总体原则

1. 测试必须对应阶段目标。
2. 单元测试可以使用手写 facts、mock data；端到端测试不得用手写 facts 替代 extraction、storage、resolution、graph 或 analysis 主链路。
3. 默认 feature 和关键 feature 组合都必须可编译。`mcp`、`all-languages` 是验收矩阵的一部分。
4. Golden fixture 用于锁定抽取输出，不用于证明产品能力已经端到端可用。
5. 新增语言、新增 schema、新增 CLI/MCP 工具、新增 analysis 能力，都必须同时补测试和文档。
6. 低置信度、启发式、fallback 行为必须在测试里显式断言 confidence、strategy 或 diagnostics。

## 2. 测试分层

### 2.1 单元测试

适用范围：
- ID 生成、enum roundtrip、range 工具函数。
- query parser、name matcher、scoring。
- 单个 builder 的局部行为。

要求：
- 可以使用内存数据和最小 fixture。
- 必须断言稳定 ID、边类型、置信度、错误分类等关键不变式。

### 2.2 抽取 Golden 测试

适用范围：
- 每种语言的 definitions、references、imports/includes、scopes、callsites、bindings、data nodes、CFG 关键输出。
- query 文件和 adapter normalize 的兼容性。

要求：
- fixture 必须是源码文件。
- expected JSON 只保留可读、稳定、对架构有约束力的字段。
- 新增语言至少包含 `simple`、`imports/includes`、`calls`、`class/method` fixtures。
- 修改 golden expected 时，必须说明是修正旧错误、能力增强，还是语法覆盖变化。

### 2.3 集成测试

适用范围：
- 多文件 extraction → store → resolution → GraphBuilder → GraphSnapshot。
- import/export、include、path alias、container、callers/callees、search/context。

要求：
- 使用真实源码 fixture。
- 使用 `Store::open_in_memory()` 可以，但必须经过正常 `insert_file_facts` 和 resolver/builder。
- 必须断言关键结果的语义。
- 涉及 incremental sync 时，必须覆盖新增、修改、删除场景。

### 2.4 CLI 测试

适用范围：`atlas init/index/sync/search/status/files/context/trace/doctor`。

要求：
- 对用户可见能力，至少要有一个真实临时项目测试实际命令路径。
- `trace` 必须验证真实源码项目经过 `index` 后能从指定位置产生变量来源和 caller path。
- `trace`、`status`、`doctor` 必须验证用户可见的语言能力边界输出。

### 2.5 MCP 测试

适用范围：MCP tool registration、schema、bounded output、错误输出、trace/query/context 工具。

要求：
- `cargo test -p atlas-cli --features "mcp"` 必须通过。
- 新增 MCP 工具必须测试注册名、required schema、正常调用和错误调用。
- 输出必须断言 bounded 行为、confidence/provenance 暴露和结构化 JSON。
- trace/query/context 工具必须断言顶层 `capability` 对象存在。

### 2.6 端到端测试

适用范围：当前阶段声明已经可用的产品能力。

要求：
- 必须从真实源码开始，经过真实入口。
- 必须经过 SQLite 持久化。
- 必须经过用户入口：CLI、MCP 或等价 public API。
- 必须断言最终用户可消费结果。
- 必须覆盖"请求超出语言能力边界"的场景。

## 3. 阶段测试要求

### P0-P4（已完成）

语义绑定、产品化索引、Resolver/GraphBuilder 分离、Binding/DataFlow 基础、CFG 基础的测试要求已完成验收。

### P5：变量来源追踪与调用路径查询 ✅

全部 14 种 DataflowFull 语言已覆盖 symbols/references/imports/calls golden fixture 和 dataflow edge/path smoke 测试。

### `atlas-engine` facade 稳定阶段

最低要求：
- `atlas-engine` facade public API 的行为保持一致。
- CLI/MCP 只调用 engine/API，不复制 resolver、graph、analysis 算法。

完成标准：
- engine crate 有独立单元和集成测试。
- CLI crate 有命令级 smoke/E2E 测试。
- MCP crate 有 tool schema、routing、bounded output 测试。
- 原有 default、all-languages、mcp 组合测试继续通过。

### Lazy UX / Analysis Contract ✅

要求：
- 触发 lazy extraction 的 MCP 工具必须断言 `analysis_contract` 存在。
- `safe_conclusions` 和 `unsafe_conclusions` 不能是泛泛提示，必须能对应到具体缺失或存在的 `CapabilityMask` bit。
- `atlas_resume(query_id)` 必须覆盖：query snapshot 存储、TTL 内恢复、未知/过期 query_id 错误、恢复后返回完整结果。
- `atlas_jobs(query_id)` 必须覆盖按查询过滤和 pending/complete/failed 状态展示。
- Investigation state 必须测试 symbol、position、field focus 对 related files/symbols 和 desired capabilities 的更新。

### Domain Rules / Lifecycle ✅

要求：
- `domain_rules` schema 测试必须覆盖新增列：`language`、`pattern_kind`、`meta`、`meta_version`、`status`、`updated_at`。
- `GenericRuleEngine` 测试必须证明 disabled/candidate/rejected/deprecated 规则不参与匹配。
- 每个 language registry 必须测试 unknown `rule_kind` 和不允许的 `pattern_kind` 会被拒绝。
- C/C++ `CppOwnershipRules` 必须测试 user/builtin/learned 规则解释、free/alloc/owned_pattern/cleanup 匹配和旧别名兼容。
- `FieldLifecycleEngine` 和 `BranchDiffEngine` 必须使用 CFG/dataflow facts 作为输入；不得用手写最终 verdict 宣称端到端能力。
- lifecycle proof 必须覆盖 rule-backed 和 incomplete 两类结果。

## 4. Feature 测试矩阵

每次合并前至少运行：

```bash
cargo test
cargo test -p atlas-cli --features "all-languages"
cargo test -p atlas-cli --features "mcp"
cargo test -p atlas-cli --features "all-languages,mcp"
```

## 5. 禁止事项

1. 禁止用手写 facts 的测试宣称端到端能力完成。
2. 禁止只断言数量大于 0 来证明 resolution、graph、trace 正确。
3. 禁止 MCP/CLI 工具只注册不测试实际调用。
4. 禁止新增 schema 表但不测试 insert/query/delete/cascade。
5. 禁止修改 golden expected 而不说明语义原因。
6. 禁止在稳定 `atlas-engine` public API 前跳过当前阶段端到端和语义精度门禁。
7. 禁止 learned domain rules 在未 approve 时影响分析结果。
8. 禁止用独立 Function IR mock 替代 CFG/dataflow facts 来证明 lifecycle 或 branch diff 能力。
