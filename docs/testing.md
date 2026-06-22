# Atlas 测试规范

本文定义不同阶段的测试深度、准入门槛和禁止事项。目标是让测试能真实约束架构演进，而不是只证明某个局部 helper 可以工作。

## 1. 总体原则

1. 测试必须对应阶段目标。
2. 单元测试可以使用手写 facts、mock data；端到端测试不得用手写 facts 替代 extraction、storage、resolution、graph 或 analysis 主链路。
3. 所有 feature 组合都必须可编译。`mcp` 是验收矩阵的一部分。
4. Golden fixture 用于锁定抽取输出，不用于证明产品能力已经端到端可用。
5. 新增语言、新增 schema、新增 CLI/MCP 工具、新增 analysis 能力，都必须同时补测试和文档。
6. 低置信度、启发式、fallback 行为必须在测试里显式断言 confidence、strategy 或 diagnostics。
7. 涉及分析等级、索引模式或用户入口的改动，必须验证所有相关代码路径；不能只验证最方便的一条 helper 或 shared pipeline。

### 1.1 分析等级与入口路径覆盖

Atlas 同时存在 extraction mode、capability level、lazy precision tier 和多个用户入口。任何改变 `Manifest`、`ResolutionSymbols`、`Structural`、`LazyDataflow`、`Full`，或改变 capability/mask/precision/status 展示的 PR，都必须明确列出并验证受影响路径。

最低路径矩阵：

| 等级/路径 | 必须验证的入口 | 必须验证的结果 |
|-----------|----------------|----------------|
| Manifest | `atlas index --analysis manifest`、shared `run_index_pipeline(Manifest)`、必要时 `atlas sync --analysis manifest` | 只写 manifest 事实；不会误报 structural/dataflow；用户可见 precision/status 正确 |
| ResolutionSymbols | dependency/lazy resolution 触发路径 | 只写 resolution symbols/imports/scopes；不会破坏已有 manifest/structural 层；stale hash 行为正确 |
| Structural | `atlas index` 默认路径、shared filesync pipeline、`atlas sync` 默认路径、`LazyStructuralService` | symbols/scopes/references/callsites 写入；resolution/graph build 正确；manifest -> structural 升级正确 |
| LazyDataflow | high-level `Engine::trace_variable`、`LazyDataflowService::ensure_for_position`、`ensure_for_function`、prebuilt full-index cache hit | unit dataflow/CFG 写入或复用正确；callsite/data-node joins 正确；budget/pending 内部状态能稳定映射为 public retry/gaps |
| Full | `atlas index --analysis full`、shared pipeline Full、`atlas sync --analysis full` | structural + dataflow + CFG + summaries 全链路持久化；file/unit extraction_state 和 capability mask 不欠报、不误报 |
| Raw analysis consumers | `RawTraceEngine`、analysis crate direct tests | 明确说明它们是否负责触发 lazy；若不触发，测试必须先准备所需 DB facts |

测试要求：
- 同一修复如果影响 shared pipeline 和 CLI 自有 pipeline，必须覆盖两者。
- 同一修复如果影响 file-level state 和 unit-level state，必须覆盖两者。
- 同一修复如果影响 lazy 和 non-lazy，必须至少有一个 lazy 回归测试和一个 full/structural 回归测试。
- capability/status/precision 的测试必须验证数据库状态和用户可见输出，不能只检查内存对象。
- 当某个路径确认不受影响时，PR 或 review 里必须写明理由。

强制回归场景：
- `run_index_pipeline(Manifest)`、CLI `atlas index --analysis manifest` 和 `atlas sync --analysis manifest` 必须覆盖“文件已删除后再次索引”的场景，断言 stale file、symbol、reference、edge 和 extraction_state 均被清理。
- `run_index_pipeline(Full)`、`atlas index --analysis full`、`atlas sync --analysis full` 必须分别断言 summary tables 已构建，并且 `summaries` capability 只在 summary build 成功后出现。
- 每种语言的 Manifest 测试必须断言只产生顶层符号。不得仅测试 query parse 成功；fixture 必须包含函数/方法内部局部定义以证明不会过度索引。
- `LazyDataflowService::ensure_for_position` 和 `ensure_for_function` 必须分别覆盖 fresh build、unit cache hit、full-index prebuilt cache hit、pending/already-building、budget partial。
- MCP `trace(kind="variable")` 必须覆盖有 path 和无 path 两种结果；只要 lazy dataflow 产生可恢复工作，两者都必须有 `query_id` 和 `analysis.retry_after_ms`，终态必须移除 retry 并保留必要 gaps。
- MCP `branch_diff` / `lifecycle` 必须覆盖 lazy CFG build 后成功分析的路径，断言 public analysis view 不会同时声明 CFG 缺失。
- CLI `--analysis` 必须覆盖合法值和非法值；非法值必须返回错误，不能静默 fallback。

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
- 新增语言或新增 Manifest 模式时，必须包含 `manifest` fixture，且源码中同时包含顶层声明和非顶层局部声明。
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

适用范围：`atlas index/sync/status/files/doctor` CLI 命令。

要求：
- 对用户可见能力，至少要有一个真实临时项目测试实际命令路径。
- `index` / `sync` 必须覆盖 analysis mode、capability state、增量新增/修改/删除和非法参数。
- `status`、`doctor` 必须验证用户可见的 schema、index mode 和语言能力边界输出。Atlas 没有 `atlas trace` CLI 命令；trace 产品路径由 MCP 和 high-level `Engine` 测试覆盖。

### 2.5 MCP 测试

适用范围：MCP tool registration、schema、bounded output、错误输出、trace/query/context 工具。

要求：
- `cargo test -p atlas-cli --features "mcp"` 必须通过。
- 新增 MCP 工具必须测试注册名、required schema、正常调用和错误调用。
- 输出必须断言 bounded 行为、confidence/provenance 暴露和结构化 JSON。
- trace/query/context 工具必须断言顶层 `capability` 对象存在。
- 触发 lazy extraction 的 MCP 工具必须断言 `analysis.retry_after_ms`、`query_id`、`gaps` 和 `resume_query` 的终态收敛语义；不得重新引入 `precision`、`work`、`lazy_diagnostics` 或 `analysis_contract`。
- 如果工具返回错误但已经触发可恢复 refinement，必须保留可操作的 `query_id` 和 retry 状态；不可恢复错误不得伪造 retry。

### 2.6 TUI 测试

适用范围：搜索状态机、command palette、后台任务、取消、终端布局和共享工具调用。

要求：
- command form 必须覆盖默认参数、当前 symbol/file/query ID 注入、字段编辑、必填校验和数值校验。
- discriminator 驱动的表单必须断言只展示和提交当前 variant/action 适用的字段，导航必须跳过隐藏字段。
- MCP-backed command 必须至少有一个测试证明它通过 `ToolRouter` 返回真实结果，而非展示层占位字符串。
- 结果投影必须覆盖 graph/impact、trace capability/confidence、source excerpt、同步空结果、管理记录、纯文本错误和未知字段前向兼容；根控制字段不得泄漏为代码事实。
- 关键 overlay 和主布局必须用 Ratatui `TestBackend` 在窄终端尺寸渲染，防止 panic 和核心操作不可见；分析结果模式必须验证主体全宽阅读区域、自适应 HUD、完整键值降级以及可见的滚动/raw/关闭提示。
- TUI 不得伪造 precision、coverage、gaps 或 pending 状态；这些字段只能来自共享 handler 响应。
- raw response 必须可达；默认 facts 视图隐藏的内容必须属于文档化的公共元数据集合，未知非元数据字段必须保留。
- 用户可取消的任务必须覆盖提交、替换、取消和 worker 回收。
- 涉及 cold-start/focus 行为的发布验证不能只使用 `TestBackend`。必须从一个已有
  manifest、仅部分 structural 的大型真实项目中启动 bare `atlas`，按用户习惯完成
  “输入符号 → Enter 搜索 → Enter 打开 → Tab 到 Source → `:explore` → Run”，并记录
  首次源码范围、终态 HUD、coverage/gaps 和退出后的 closure coverage。自动化测试负责
  可重复语义，人工 TUI smoke 负责验证真实交互链路。

### 2.6.1 Focus/closure 回归矩阵

- 冷 C/C++ class/struct/enum 的首次详情和首次 `explore` 必须返回完整 defining scope，
  不能先返回一行再依赖 `resume_query` 修正。
- 上述测试必须同时覆盖 manifest-only 冷文件和“content hash 相同、structural state 标记
  complete、但 type range 来自旧抽取语义”的缓存文件；后者必须被不变量检查拒绝并
  自愈重抽。
- symbol seed 的 call/type 扩展必须排除同文件无关 peer；测试 fixture 应同时包含相关和
  无关调用，证明闭包不是 file-wide fan-out。
- import/include dependency 默认只产生 `resolution_symbols` coverage；未被 call/type 关系
  选中的 header 不得计入 structural closure。
- incoming cold caller 必须覆盖“候选发现 → structural extraction → scoped resolution →
  verified edge”，且候选发现本身不得当作调用边。
- 后台成功物化的文件必须进入 resume 时的 graph refresh；后台失败必须退出 pending、
  保留诊断并形成终态 gap。
- `calls`/`path` 的前台 closure 测试必须断言只物化 seed；请求 depth 只影响可追踪后台
  fixed point。真实大文件 smoke 必须分别记录首次旧缓存自愈和第二次热缓存结果。
- missing file、取消和 extraction error 不得计入 closure files 或完整 coverage。
- C/C++ multiline `enum` 与 struct/class 一样必须覆盖完整 defining scope；旧的一行 enum
  缓存必须只重建一次，第二次访问不能再次判定 stale。
- `lifecycle` 回归必须直接使用抽取出的 CFG/dataflow，在查询时组合 effects，并分别覆盖
  field 与 local resource。Linux fixture 至少验证 `kzalloc_obj`/`kfree` 分类和一个真实 TUI
  流程（例如 `vga_arb_open::priv`），不能用手工填充最终 transition 代替。

### 2.7 端到端测试

适用范围：当前阶段声明已经可用的产品能力。

要求：
- 必须从真实源码开始，经过真实入口。
- 必须经过 SQLite 持久化。
- 必须经过用户入口：CLI、MCP 或等价 public API。
- 必须断言最终用户可消费结果。
- 必须覆盖"请求超出语言能力边界"的场景。

### 2.8 发布前验证矩阵

发布候选至少运行：

```bash
cargo fmt --all -- --check
cargo check --workspace --all-features
cargo test --workspace --all-features
```

如果某个 crate 支持无语言 feature 编译，则默认 feature 的单测也必须通过；否则需要在 Cargo feature 或测试上明确表达“至少一个语言 feature 是前置条件”。

发布前还必须保存一份路径级验证记录，至少列出：
- 本次变更影响的 extraction modes、capability bits、lazy precision/status、用户入口。
- 已验证的 CLI、MCP、TUI、shared pipeline、sync、lazy、raw analysis 路径。
- 未受影响路径及理由。
- 所有失败测试、跳过测试和 residual risk。

### 2.9 管线等价性测试

同一项目通过不同入口（CLI index、sync、shared `IndexPipeline`）索引后必须产生相同 DB 状态。
测试使用 in-memory `Store` + 临时项目 + `ExtractionMode::Structural` / `Full`，
断言 files、symbols、edges、summaries 及 `extraction_state` 等价。
覆盖新增、修改、删除场景。

### 2.9 多语言 Feature Matrix

每种语言至少覆盖以下 compile-time / runtime 验证链：
`from_extension` → `create_frontend` → manifest query → structural query →
search `lang:` prefix → `CapabilityProfile::all_compiled()` → golden fixture smoke。
所有 14 种语言已默认编译，必须全部纳入矩阵。

### 2.10 清理与架构收敛 PR 门禁

清理类 PR 不能只证明“代码少了”，必须证明行为和架构边界没有漂移。

最低要求：
- 删除代码前必须确认零生产调用点、零测试支撑用途，或明确替代路径；测试 helper 不得按死代码处理。
- 抽取 helper 或 builder 时，必须至少覆盖一个最简单调用点和一个有分支/merge 的调用点，防止共享抽象只适用于 happy path。
- MCP lazy response 迁移必须断言已删除的 `precision`、`hint`、`work`、`lazy_diagnostics` 和 `analysis_contract` 不会重新出现，并覆盖 `analysis.retry_after_ms`、结构化 `gaps`、`query_id`、`tasks(query_id)` 和 `resume_query` snapshot 语义。
- stable facade API 重构必须有编译级兼容验证。若旧 API 接受闭包、函数指针或常见 wrapper，新 trait/API 必须保留等价调用方式，或在文档中声明 breaking change。
- 每个清理批次至少运行 `cargo fmt --check`、`cargo check` 和受影响 crate 的测试；如果全量 `cargo test` 存在已知失败，PR/review 必须列出具体失败测试、原因和是否与本次变更相关。

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
- 原有 default、mcp 组合测试继续通过。

### Lazy UX / Public Analysis View ✅

要求：
- 触发 lazy extraction 的 MCP 工具必须通过统一 `analysis` 视图暴露 scope、basis、summary 和仅在可继续提升时存在的 `retry_after_ms`。
- 终态缺口必须映射为公开 `{scope, reason, detail}`，不得直接序列化内部枚举。
- `resume_query(query_id)` 必须覆盖：query snapshot 存储、TTL 内恢复、未知/过期 query_id 错误、恢复后返回完整结果。
- `tasks(query_id)` 必须覆盖按查询过滤和 pending/complete/failed 状态展示。
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
cargo test -p atlas-cli --features "mcp"
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
