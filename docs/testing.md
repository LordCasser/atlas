# Atlas 测试规范

本文定义**当前**测试深度、准入门槛与强制回归。只写现行规则与覆盖矩阵；版本变更见 [`CHANGELOG.md`](../CHANGELOG.md)。

## 1. 总体原则

1. 测试必须对应阶段目标。
2. 单元测试可以使用手写 facts、mock data；端到端测试不得用手写 facts 替代 extraction、storage、resolution、graph 或 analysis 主链路。
3. 所有 feature 组合都必须可编译。`mcp` 是验收矩阵的一部分。
4. Golden fixture 用于锁定抽取输出，不用于证明产品能力已经端到端可用。
5. 新增语言、新增 schema、新增 CLI/MCP 工具、新增 analysis 能力，都必须同时补测试和文档。
6. 低置信度、启发式、fallback 行为必须在测试里显式断言 confidence、strategy 或 diagnostics。
7. 涉及分析等级、索引模式或用户入口的改动，必须验证所有相关代码路径；不能只验证最方便的一条 helper 或 shared pipeline。

### 1.1 分析等级与入口路径覆盖

Atlas 术语分层见 `docs/architecture.md` §1.1：`ExtractionMode`（L2）、`CapabilityLevel`/`FeatureMatrix`（L0）、`FactCoverage`/CatalogTier（L1）、`AccessStrategy`/`PipelineGrade`/`EdgeProvenance`（L3）、内部 **`AnswerQuality`（L4）**。任何改变 `Manifest`、`ResolutionSymbols`、`Structural`、`LazyDataflow`、`Full`，或改变 capability/mask/AnswerQuality/status 展示的 PR，都必须明确列出并验证受影响路径。不得再引入第二个名为 `IndexMode` 的类型。

**产品路径（查询时）：** 对外只验证 **Index（预物化 → FullCache）** 与 **Focus（意图局部加强）**。按需 structural/dataflow ensure 属于 **Focus materialize**（内部机制；类型名可含 `Lazy*`），测试可调用 ensure API，但不得把 “Lazy 产品线” 写成与 Focus 并列的第三入口。

最低路径矩阵：

| 等级/路径 | 必须验证的入口 | 必须验证的结果 |
|-----------|----------------|----------------|
| Manifest | `atlas index --analysis manifest`、shared `run_index_pipeline(Manifest)`、必要时 `atlas sync --analysis manifest` | 只写 manifest 事实；不会误报 structural/dataflow；用户可见 status/catalog 正确 |
| ResolutionSymbols | dependency / Focus dependency 物化路径 | 只写 resolution symbols/imports/scopes；不会破坏已有 manifest/structural 层；stale hash 行为正确 |
| Structural | `atlas index` 默认路径、shared filesync pipeline、`atlas sync`、`FocusMaterialize`/`LazyStructuralService` | symbols/scopes/references/callsites 写入；resolution/graph 正确；manifest → structural 升级正确 |
| LazyDataflow（L2；Focus materialize 内部） | `Engine::trace_variable`、`FocusMaterialize`/`LazyDataflowService::ensure_*`、full-index unit cache hit | unit dataflow/CFG 写入或复用；callsite/data-node joins；budget/pending → public retry/gaps；**与 Focus 控制面同一 materialize 配置** |
| Full | `atlas index --analysis full`、shared pipeline Full、`atlas sync --analysis full` | structural + dataflow + CFG + summaries 全链路；extraction_state / FactCoverage 不欠报、不误报 |
| Raw analysis consumers | `RawTraceEngine`、analysis crate direct tests | 明确是否触发 materialize；若不触发须先准备 DB facts |
| N5 邻域对拍 | 见 §2.6.2 | Focus complete 文件/unit 切片 ≈ Index 同范围 |

测试要求：
- 同一修复如果影响 shared pipeline 和 CLI 自有 pipeline，必须覆盖两者。
- 同一修复如果影响 file-level state 和 unit-level state，必须覆盖两者。
- 同一修复如果影响 Focus materialize ensure 与 Index 预物化，必须至少各有一条回归（禁止只测 helper）。
- 所有已编译语言至少进入一条 Focus function-unit 与 full Index 的 bindings、dataflow（含 edge kind/confidence）、CFG
  对拍；共享基线矩阵覆盖普通函数边界，语言特有语义继续使用独立 fixture，不能用基线
  测试替代 type-switch、mixed short declaration、Go select receive、match binding、PHP nested/keyed
  destructuring、全部 14 种语言身份的 supported direct-variable mutation、Ruby multiple assignment、Java guarded type/record pattern、C# parenthesized nested designation、Kotlin
  late-assignment branch provenance、TypeScript-family `let/const` declaration
  destructuring block binding/aggregate provenance、assignment destructuring
  existing-binding reuse/aggregate provenance、parameter destructuring
  function binding/shared argument position/summary-and-Focus `ArgToParam`、
  `for-of`/`for-in` simple/nested pattern loop binding/aggregate provenance、
  Cangjie simple/nested-tuple/enum-payload
  `for-in` loop binding/aggregate provenance、modifier loop、nested lexical shadowing
  等精确断言。
- 声明 `scope_aware_binding` 前必须同时具备三层证据：直接 extraction 断言 distinct
  `BindingId`/`scope_id` 以及 `BindingUse`/`DataNode.binding_id`，SQLite Trace 断言持久化后
  sink identity 与 Assign path，Focus cold unit 对 full Index 的 bindings/dataflow/CFG 对拍。
  当前共享矩阵覆盖 TypeScript、JavaScript、ArkTS、Java、C、C++、Go、Rust、Kotlin、
  Cangjie、PHP、Ruby；Python/C# 及其他 pattern/namespace 特化语言继续保留各自 fixture。Java 必须用语言
  合法的 sibling block，不能用编译器拒绝的 overlapping local redeclaration 伪造
  shadowing。
- capability/status 测试必须验证数据库状态和用户可见输出，不能只检查内存对象。
- 当某个路径确认不受影响时，PR 或 review 里必须写明理由。
- FullCache/Focus 判定必须覆盖：整仓 finalized manifest + 少量 Focus structural、
  整仓 finalized structural + 少量 Focus dataflow、任一 scoped Index、以及 stale/
  incomplete file layer。断言必须按 QueryNeed 分开，不能拿聚合 CatalogTier 代替
  scope-wide fresh complete per-file coverage。另须覆盖源码变化后的 Focus structural
  rebuild：Structural 可保持 FullCache，但含 reference 的变化文件或仍指向其旧 symbol
  的调用方必须失效 current resolution fingerprint，使 CallGraph 回到 Focus；无 reference
  且未被引用的文件不得因此永久降级。
- MCP/TUI 一致性改动必须覆盖同一工具的 `ToolContract` 与 handler required need、
  pending→resume、background failure→`tasks(status=failed)`、单块有效 JSON、TUI 表单
  默认值/array 参数，以及 ToolRouter 写后 native GraphSession stale + 状态栏刷新。
- 冷 `search` scope 含多个 inventory-only 候选时，必须断言每个 deferred 文件都有
  可追踪的 Focus seed；`tasks(status=ready)` 后一次 `resume_query` 必须进入无 retry 的
  终态，不得只用三个以内文件的同目录 fixture 掩盖 inventory/files 扩展差异。
- `file_dependencies(manifest)` 必须断言不启动 Focus；
  `file_dependencies(structural)` 必须断言 CallGraph Focus 能收敛到跨文件依赖。

强制回归场景：
- `run_index_pipeline(Manifest)`、CLI `atlas index --analysis manifest` 和 `atlas sync --analysis manifest` 必须覆盖“文件已删除后再次索引”的场景，断言 stale file、symbol、reference、edge 和 extraction_state 均被清理。
- `run_index_pipeline(Full)`、`atlas index --analysis full`、`atlas sync --analysis full` 必须分别断言 summary tables 已构建，并且 `summaries` capability 只在 summary build 成功后出现。
- 每种语言的 Manifest 测试必须断言只产生顶层符号。不得仅测试 query parse 成功；fixture 必须包含函数/方法内部局部定义以证明不会过度索引。
- `LazyDataflowService::ensure_for_position` 和 `ensure_for_function` 必须分别覆盖 fresh build、unit cache hit、full-index prebuilt cache hit、pending/already-building、budget partial。
- unit 写库 FK：`filter_data_nodes` — 无效 `binding_id` 必须 **SET NULL 保留节点**；无效 `function_id` 才丢弃（`db::fk_guards` 单测）。
- unit `CALL_EDGES` capability 只有在 structural fresh 且目标 unit 存在真实 callsite 时置位；无调用函数必须保持 unset。
- MCP `trace(kind="variable")` 必须覆盖有 path 和无 path 两种结果；只要 materialize 产生可恢复工作，两者都必须有 `query_id` 和 `analysis.retry_after_ms`，且非终态 JSON 不得包含 trace/result/partial_result 数据；终态必须移除 retry 并保留必要 gaps。
- MCP `branch_diff` / `lifecycle` 必须覆盖按需 CFG build 后成功分析的路径，断言 public analysis view 不会同时声明 CFG 缺失；编排必须经 `AnalysisRuntime::run_*`（见 §2.11）。
- CLI `--analysis` 必须覆盖合法值和非法值；非法值必须返回错误，不能静默 fallback。
- MCP 单栈：`ActiveProject` / Engine / FocusRuntime / AnalysisRuntime `same_stack_as`（或等价指针相等）。

## 2. 测试分层

### 2.1 单元测试

适用范围：
- ID 生成、enum roundtrip、range 工具函数。
- query parser、name matcher、scoring。
- 单个 builder 的局部行为。
- **源文件编码解码**（`workspace::source_text`）：见 §2.1.1。

要求：
- 可以使用内存数据和最小 fixture。
- 必须断言稳定 ID、边类型、置信度、错误分类等关键不变式。

#### 2.1.1 源编码与统一读入口（强制）

任何改动 `workspace::read_source` / `decode_source`、hash 语义或源码读盘路径的 PR，必须通过：

```bash
cargo test -p workspace --lib source_text
cargo test -p extraction --test source_encoding_extract
cargo test -p filesync --test source_encoding_index
```

| 层 | 位置 | 覆盖 |
|----|------|------|
| 单元 §2.1 | `workspace::source_text` tests | UTF-8 / GBK / windows-1252、双 hash、不写回 |
| 集成 §2.3 | `extraction/tests/source_encoding_extract.rs` | GBK 盘文件 → decode → tree-sitter 中文符号名 |
| 集成 §2.3 | `filesync/tests/source_encoding_index.rs` | index `content_hash=raw`、dirty 不永久脏、中文符号入库 |

最低断言（禁止删弱）：

| 场景 | 必须断言 |
|------|----------|
| UTF-8 | 中文保留；`file_hash == text_hash`（全文） |
| GBK 中文 | 非 UTF-8 raw 解码出预期汉字；`file_hash == blake3(raw)`；`file_hash != text_hash` |
| ISO-8859-1 系 | 西欧 8-bit 非 UTF-8 可解码；encoding 名可为 ISO-8859-1 或 windows-1252 |
| 不写回 | `read_source` 后磁盘字节与写入时一致 |
| 部分内容 hash | `text_content_hash(utf8_slice)` 基于解码后字节，不等于 raw file hash（GBK fixture） |
| 解析联调 | GBK Python 经 extract 后符号名含正确中文；`FileFacts.content_hash` 为 raw |
| index/dirty | DB `content_hash` 与 raw 重算一致；二次 index 不改为 text_hash |

产品路径回归：新增「读项目源文件」代码不得使用 `std::fs::read_to_string`，也不得
自行实现编码检测。审查命令：

```bash
rg 'read_to_string' crates/atlas-engine crates/atlas-mcp --glob '*.rs'
```

允许项仅限 `.atlasignore` / path-alias 等 UTF-8 配置、测试 fixture、MCP handler
自检；dirty / fingerprint 只计算文件身份时可直接读 raw bytes，但必须保持
`file_content_hash` 语义。

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
- re-export 测试必须覆盖 source name 与 outward name 不同的 named alias、default import/export
  以及 wildcard chain，并断言最终 `Calls`/`Instantiates` 指向源定义，不能只断言存在任意 edge。
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
- 18 秒门限测试必须覆盖两支：期限内 tracked job 完成时自动重放并只返回终态结果；期限到达时只返回票据，禁止保留 handler 的临时数组、路径、caller 或 trace body。
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
- 大型真实项目的 TUI smoke 必须在 graph 尚未加载时立即提交搜索，验证结果先于 snapshot
  完成出现；随后 Enter 打开详情，验证 running 状态可持续渲染并在后台加载完成后自动进入
  detail。只测预热 graph 的路径不能证明启动交互无阻塞。
- worker replacement 测试必须约束 `submit()` 不固定 sleep；旧 worker 通过 cancel token
  合作退出，UI 线程只负责替换 handle。

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
- 跨语言 multiline `enum` 与 struct/class/interface/trait 一样必须覆盖完整 defining
  scope；旧的一行 brace-type 缓存必须只重建一次，第二次访问不能再次判定 stale。
- `lifecycle` 回归必须直接使用抽取出的 CFG/dataflow，在查询时组合 effects，并分别覆盖
  field 与 local resource。Linux fixture 至少验证 `kzalloc_obj`/`kfree` 分类和一个真实 TUI
  流程（例如 `vga_arb_open::priv`），不能用手工填充最终 transition 代替。

### 2.6.2 N5：Focus 邻域 facts ≈ Index 同文件/同 unit

产品主张「闭包内体验 ≈ 该邻域已被 Index」必须有切片对拍，不是全库 bitwise 相等。

强制回归（`crates/atlas-cli/tests/focus_materialize_e2e.rs`）：

- **Structural neighborhood**（`n5_focus_structural_neighborhood_matches_index`）  
  - 多文件 TS fixture：`seed` 调用 `math`，`peer` 无关。  
  - Index：`--analysis structural`。  
  - Focus：manifest → `FocusMaterialize` **batch** ensure `seed`+`math`（同一 resolve/graph 批次）。  
  - 断言：seed/math 的 file structural 切片（symbols/refs/callsites/intra edges）与 Index 相等；邻域跨文件边相等；peer **无** structural complete。
- **Dataflow unit**（`n5_focus_dataflow_unit_matches_index_full`）  
  - 自包含 seed 函数（无 callee）+ peer。  
  - Index：`--analysis full`。  
  - Focus：structural 底库 → `ensure_for_function(seed)`。  
  - 断言：seed unit dataflow（含 edge kind/confidence）/CFG 切片 == Index full 同 unit；peer 无 dataflow。
- **Language-specific unit semantics**
  - Java `n5_focus_java_pattern_bindings_match_index_full` 覆盖 supported
    `if`-condition `instanceof` 与 Java 21 arrow switch type/record capture 的
    rule-local identity、0.75 tested-value/selector aggregate flow、edge confidence
    parity 与 peer method 冷态；colon group 和其他 flow-sensitive boolean context
    由直接 boundary fixture 固定为保守，不能由普通函数基线替代。
  - C# `n5_focus_csharp_pattern_bindings_match_index_full` 覆盖 parenthesized nested
    designation 的 binding/dataflow/CFG 切片、0.72 aggregate subject flow，以及 peer
    method 保持冷态；不能由普通函数基线替代。
  - Go `n5_focus_go_select_receive_dataflow_matches_index_full` 覆盖 `:=` clause-local
    declaration、`=` outer-binding reuse、blank filtering、0.78 receive aggregate flow、
    confidence parity 与 peer unit 冷态；不能由普通函数基线替代。
- **Dataflow expanded window**（`n5_focus_dataflow_expanded_window_matches_index_full`）  
  - seed 调用 math；ensure(seed) 展开 callee unit。  
  - 断言：seed 与 callee 两 unit 切片均 == Index full；peer 无 dataflow。
- **FocusRuntime prepare**（`n5_focus_runtime_prepare_structural_neighborhood`）  
  - manifest → `FocusRuntime::prepare(Calls useAdd)`。  
  - 断言：Focus 路径；seed（及若物化的 math）structural 切片可与 Index 对拍；peer 不 structural-complete。

切片键不得包含 job id、wall-clock 或仅运行时字段。禁止只比全库 `symbol_count`。

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
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test -p atlas-mcp --no-default-features --test feature_propagation
cargo test -p atlas-mcp --no-default-features --features javascript --test feature_propagation
cargo test -p atlas-mcp --no-default-features --features arkts --test feature_propagation
```

无语言 feature 必须可编译且 `enabled_languages()` 为空。共用 grammar 的单语言构建
（至少 JavaScript-only 与 ArkTS-only）必须证明不会顺带启用 TypeScript 发现/frontend。

发布前还必须保存一份路径级验证记录，至少列出：
- 本次变更影响的 extraction modes、capability bits、lazy precision/status、用户入口。
- 已验证的 CLI、MCP、TUI、shared pipeline、sync、lazy、raw analysis 路径。
- 未受影响路径及理由。
- 所有失败测试、跳过测试和 residual risk。

Release-gate policy:
- Workspace-wide, all-target, all-feature Clippy must pass with `-D warnings`.
- Local verification is macOS arm64 only; Linux and Windows coverage is via the gated release matrix (actionlint is not available locally).
- When schema is unchanged but extraction semantics change (e.g., ArkUI recovery rewrite), existing `.atlas` DBs must be removed and re-indexed — `doctor` cannot detect this via source hashes alone.

### 2.8.1 性能改动的结果等价门禁

**性能优化不得改变索引结果。** 任何以性能为目的的改动（查询重写、索引时序、
缓存、并发、批处理）在合入前必须在冷 `.atlas` 上做 A/B，并逐项比对：

- `resolution.summary` 的 `s1..s6`、`miss`、`total`；
- `s6_breakdown` 的 `s6_exact`、`s6_fuzzy_prox`、`s6_fuzzy_global`；
- `graph.build_all` 的 `edges_built`；
- 必要时直接用 `sqlite3` 读 `symbol_edges` / `data_nodes` / `dataflow_edges`
  的行数。

任何一项不一致即视为回归，无论耗时是否下降。基线与既往被否决的实验记录在
[`docs/performance.md`](performance.md)；重复一个已被实测否定的方案之前，先读
该文档的 Rejected Optimizations 表。

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

### 2.10 Post-extract hooks（index / lazy 共用）

语言特化后处理（当前：Linux C 的 `EXPORT_SYMBOL` / `module_init` / `SYSCALL_DEFINE`）必须挂在
`extraction::apply_post_extract_hooks`，并由 `extract_file_with_mode` 在所有成功返回路径调用。
禁止在 `LazyStructuralService`、Focus bootstrap 或 CLI index 旁路再挂第二份 hook。

最低测试要求（`extraction` crate，`feature = "c"`）：

| 场景 | 入口 | 必须断言 |
|------|------|----------|
| Structural 全量增强 | `extract_file_with_mode(..., Structural)` | `EXPORT_SYMBOL`/`_GPL` → `sym.exported` + `facts.exports`；`module_init` → `RegistersCallback` + `Provenance::Heuristic` |
| ResolutionSymbols | 同上，mode=`ResolutionSymbols` | `EXPORT_SYMBOL` 仍标记 exported（lazy 依赖 bootstrap 与 index 一致） |
| Manifest | 同上，mode=`Manifest` | 顶层符号仍可被 hook 标记 exported |
| 路径确定性 | 同一源码连抽两次 Structural | exported 集合与 initcall 边计数一致（index/lazy 共用路径的 parity 守卫） |

### 2.11 MCP DEBT-8：handler 纯度与 analysis dispatcher

**目标**：`lifecycle` / `branch_diff` / `impact(semantic=true)` 的能力门控、store I/O、effect composition、引擎调用归
`AnalysisRuntime`；handler 只做 arg 解析 + 符号解析 + envelope 渲染。禁止「改名 facade」
（engine 名字藏进 runtime，但 orchestration 仍在 handler）。

强制回归（`atlas-mcp` lib）：

| 场景 | 入口 / 测试 | 必须断言 |
|------|-------------|---------|
| 路由 | `contract_for` + `e2e_semantic_analysis_routes_correctly` 等 | `lifecycle`/`branch_diff` → `ToolContract::SemanticAnalysis` → 非 Unknown tool |
| Engine 名纯度 | `handler_purity_analysis_handlers_have_no_engine_hits` | `lifecycle`/`branch_diff`/`graph` 无 `FieldLifecycleEngine::` / `BranchDiffEngine::` |
| Orchestration 模式 | `handler_purity_analysis_tools_no_orchestration_in_handlers` | analysis tool handler 无 `find_cfg_*` / `find_data_nodes_*` / `compose_effects(` / `CfgGraph::build` / `CppOwnershipRules::load_for` / runtime helper 拼装等；graph semantic impact 只调 `run_semantic_impact` |
| Allowlist 卫生 | `handler_purity_no_new_direct_service_calls` + shrink 断言 | 新命中 fail；allowlist 只缩；**残量 entry 必须仍有真实 FORBIDDEN 命中** |
| Dispatcher 能力门控 | `capability_gate_rejects_non_cpp_language` / `accepts_c_and_cpp` | TS → `UnsupportedLanguage`；C/C++ 通过 |
| Dispatcher 执行 | `run_lifecycle` / `run_branch_diff` cfg-unavailable 等 | 无 CFG → 结构化错误；非仅路由 smoke |
| Semantic impact dispatcher | `semantic_impact_*` + `test_handle_impact_response_has_direction` | branch asymmetry 在 runtime 汇总；字段顺序确定；持久化 C/C++ effect rule 与默认 config 合并进入 composer；公开 JSON 结构不漂移 |
| Lifecycle 语言边界 | `lifecycle_unsupported_language_returns_terminal_gap` | 非 C/C++ → `unsupported_language` gap，非 panic |
| calls 1-hop 契约 | `callers_depth_param_*` / `callees_with_depth_gt_1_*` / `callers_include_signature_*` | depth 警告；多跳 depth 仍 1-hop；signature 来自 store |
| Focus⊆Index 邻域 | §2.6.2 N5 + `focus_equivalence_vs_full_index` | Focus 冷路径与 Full 基线多维可比 |
| Cross-fn Phase2 | `focus_mode_phase2_arg_to_param_without_summary` | **无 summary** 时 `RuntimeEdgeProvider` 仍产出 `ArgToParam`（防误删 Phase2） |
| ArkTS AppStorage | `arkts_app_storage_bridges_writer_value_to_web_bound_field` + `cold_arkts_trace_materializes_cross_directory_appstorage_writer` | `set/setOrCreate` value 通过精确 key 匹配到 `StorageProp/StorageLink` 字段与 UI `CallArg`；literal/expression 与 receiver 不得误合并；冷 Focus 跨目录 writer 经 resume materialize 后必须出现 `StateFlow` |
| ArkTS declaration/search | `declaration_recovery_preserves_navigation_component_and_following_builder` + `execute_decorator_query_refines_mixed_manifest_and_structural_scope` | 深层 ArkUI DSL 不得吞 owning struct/build/后续 Builder；混合 manifest/structural scope 的 exact decorator search 必须补齐 structural facts 后再声明 complete/total |
| JS/TS/ArkTS direct-variable mutation | `test_typescript_family_variable_mutations_preserve_read_modify_write_provenance` + `fx_typescript_family_variable_mutations_persist_and_trace_read_modify_write_inputs` + `n5_focus_typescript_family_variable_mutations_match_index_full` | 三种 language identity 各自验证 direct identifier `op=`/`++`/`--` 的 previous-value/RHS→aggregate Expr、Expr→Local(0.90)、SQLite/Trace 与 Focus==full Index；member/subscript target 保持负边界 |
| JS/TS/ArkTS logical assignment | `test_typescript_family_logical_assignments_preserve_may_provenance` + `fx_typescript_family_logical_assignments_persist_and_trace_may_provenance` + `fx_typescript_real_opencode_nullish_assignment_persists_and_traces` + `n5_focus_typescript_family_logical_assignments_match_index_full` | 三种 language identity 各自验证 direct identifier `&&=`/`||=`/`??=` 的 old-value/RHS→aggregate Expr(Read 0.75)、Expr→Local(Assign 0.90)、SQLite/Trace 与 Focus==full Index；真实 OpenCode `defaultPreferred ??=` 锁定 CallTarget 来源；不得推导 RHS 必然执行，member/subscript target 保持负边界 |
| Shared reaching definition / Trace value selection | `test_resolve_use_def_activates_assignment_after_rhs` + `fx2_multi_assignment_chain_complete` + `sem_c_multi_assignment_chain_complete` + `vfy_ts_canonical_provenance_path_field_to_return` | 写入只在显式 RHS 求值结束后激活；`x = x + 1` 的 RHS 读取旧 definition、后续 use 读取新 definition；线性 Trace 在多操作数表达式中优先保留 CallTarget→ArgToCall 桥，再延续 state-bearing source，不因字面量提前终止；这是 source-order approximation，不冒充 CFG-aware SSA |
| JS/TS/ArkTS declaration destructuring | `test_typescript_family_declaration_destructuring_preserves_bindings_and_aggregate_flow` + `fx_typescript_family_declaration_destructuring_persists_and_traces_initializers` + `fx_typescript_real_opencode_declaration_destructuring_persists_and_traces` + `n5_focus_typescript_family_declaration_destructuring_matches_index_full` | 三种 language identity 各自验证 `let/const` simple/renamed/nested/default/rest target 的 block-scoped binding、outer shadow 恢复、whole initializer→target Assign(0.85)、computed key/default RHS 读取、SQLite/Trace 与 Focus==full Index；真实 OpenCode `{ id: _, sessionID: __, ...rest } = info` 锁定 persisted evidence chain；`var` 与 exact property/index projection 保持负边界 |
| JS/TS/ArkTS assignment destructuring | `test_typescript_family_assignment_destructuring_reuses_bindings_and_aggregate_flow` + `fx_typescript_family_assignment_destructuring_persists_and_traces_rhs` + `fx_typescript_real_opencode_assignment_destructuring_persists_and_traces` + `n5_focus_typescript_family_assignment_destructuring_matches_index_full` | 三种 language identity 各自验证 object/array simple/renamed/nested/default/rest identifier target 复用已有 binding、不新增 BindingDef，whole RHS→target Assign(0.85)，computed key/default RHS 保持读取且 target 不伪造 read；SQLite/Trace、Focus==full Index 与 peer cold isolation 对齐；真实 OpenCode `;[y, m] = shift(y, m, -1)` 锁定 assignment 后 use 的 RHS evidence；exact property/index projection、default activation、member/subscript target 与 parallel evaluation order 保持负边界 |
| JS/TS/ArkTS parameter destructuring | `test_typescript_family_parameter_destructuring_preserves_bindings_and_argument_positions` + `test_typescript_parameter_positions_exclude_the_erased_this_parameter` + `fx_typescript_family_parameter_destructuring_persists_and_traces_call_arguments` + `fx_typescript_real_opencode_parameter_destructuring_persists_and_traces` + `n5_focus_typescript_family_parameter_destructuring_matches_index_full` | 三种 language identity 各自验证 function/method/arrow simple/renamed/nested/default/rest leaf 的 function-scoped Parameter binding 与共享顶层 argument position；TypeScript erased `this` 不消耗 runtime argument；Full summary 与 cold Focus runtime 均用 whole call argument 生成 aggregate `ArgToParam`；computed key/default RHS 读取、SQLite/Trace、Focus==full Index 与真实 OpenCode `hasFunctionCall` 跨文件调用对齐；exact property/index projection 与 parameter default activation 保持负边界 |
| JS/TS/ArkTS for-of/for-in binding | `test_typescript_family_for_in_bindings_receive_iterable_aggregate` + `fx_typescript_family_for_in_bindings_persist_and_trace_from_iterables` + `fx_typescript_real_opencode_for_of_pattern_persists_and_traces` + `n5_focus_typescript_family_for_in_bindings_match_index_full` | 三种 language identity 各自验证 `let/const` simple/nested pattern 的 loop-scoped binding、outer shadow 恢复、existing-local assignment reuse、whole iterable/object→target Assign(0.65)、SQLite/Trace 与 Focus==full Index；真实 OpenCode `[key, value] of entries` 锁定中间证据链；property key/`var` binding、member/subscript target、exact element/key projection 与 async scheduling 保持负边界 |
| C/C++/Java/C# direct-variable mutation | `test_c_style_variable_mutations_preserve_read_modify_write_provenance` + `fx_c_style_variable_mutations_persist_and_trace_read_modify_write_inputs` + `n5_focus_c_style_variable_mutations_match_index_full` | 四种不同 AST identity 各自验证 direct identifier compound assignment 与 `++`/`--` 的 previous-value/RHS→aggregate Expr(0.75)、Expr→Local(0.90)、RHS-only write suppression、SQLite/Trace operator selection、Focus==full Index 与 cold peer isolation；member/field、subscript/element/array、pointer target 保持负边界 |
| PHP direct-variable mutation | `test_php_variable_mutations_preserve_read_modify_write_provenance` + `fx_php_variable_mutations_persist_and_trace_read_modify_write_inputs` + `n5_focus_php_variable_mutations_match_index_full` | file/function/method variable `op=`/`++`/`--` 共用 callable binding；previous-value/RHS→aggregate Expr(0.75)、Expr→coalesced Local(0.90)，并验证 SQLite/Trace、Focus==full Index、cold peer isolation；dynamic/non-variable target 与 `??=` 保持负边界 |
| Python/Go/Rust/Kotlin/Ruby direct-variable mutation | `test_remaining_language_variable_mutations_preserve_read_modify_write_provenance` + `fx_remaining_language_variable_mutations_persist_and_trace_read_modify_write_inputs` + `n5_focus_remaining_language_variable_mutations_match_index_full` | 五种 pinned grammar identity 各自验证支持的 direct identifier augmented/compound/operator/update form 的 previous-value/RHS→aggregate Expr(0.75)、Expr→Local(0.90)、RHS-only write suppression、SQLite/Trace operator selection、Focus==full Index 与 cold peer isolation；attribute/selector/field/navigation、subscript/index、receiver/pointer/dereference target 以及 Ruby `||=` 保持负边界 |
| Rust parameter pattern | `test_rust_parameter_patterns_preserve_bindings_scopes_and_runtime_positions` + `test_explicit_parameter_positions_leave_non_runtime_parameters_unmapped` + `fx_rust_parameter_patterns_persist_and_trace_aggregate_call_arguments` + `fx_rust_real_focus_engine_closure_parameter_pattern_persists_and_traces` + `n5_focus_rust_parameter_patterns_match_index_full` | named function/method tuple/struct/ref pattern leaf 验证 Parameter identity、共享顶层 `arg_index` 与 whole-argument `ArgToParam`；`self` 不占 runtime argument；closure identifier/mut/reference/tuple parameter 验证独立 function scope、本地 use-def、无 enclosing named-call position；SQLite/Trace、真实 `FocusEngine` tuple closure、cold Focus==full Index 与 peer isolation 对齐；exact component projection、receiver→`self` 与 closure invocation resolution 保持负边界 |
| Cangjie assignment mutation | `test_cangjie_variable_reassignment_and_mutations_preserve_provenance` + `fx_cangjie_variable_reassignment_and_mutations_persist_and_trace_inputs` + `n5_focus_cangjie_variable_reassignment_and_mutations_match_index_full` | direct simple reassignment 验证 RHS→Local(0.90) 与 LHS read suppression；direct identifier non-conditional compound/postfix update 验证 previous-value/RHS→aggregate Expr(0.75)、Expr→Local(0.90)、SQLite/Trace operator selection、Focus==full Index 与 cold peer isolation；field/index target、`&&=`/`||=` 保持负边界 |
| Rust structural pattern projection | `test_rust_match_and_guard_let_bindings_use_structural_projection_paths` + `fx_rust_structural_pattern_projections_persist_and_trace_exact_inputs` + `fx_rust_real_closure_planner_nested_result_projection_persists_and_traces` + `n5_focus_rust_structural_pattern_projections_match_index_full` | match scrutinee/guard-let RHS 对 fixed tuple/tuple-struct/struct/slice-prefix capture 验证 anonymous Expr access path、FieldLoad(0.80)→Assign(0.90)、SQLite/Trace 与 Focus==full Index；真实 `ClosurePlanner::plan_dependencies` 锁定 nested `Result<Option<_>>` 的 `[0][0]`；bare/`@` whole capture、`..` 后 target 的 aggregate Assign(0.75) 与 cold peer isolation 保持显式边界 |
| Rust ordinary let pattern | `test_rust_let_pattern_bindings_activate_after_declaration_and_project_values` + `fx_rust_let_patterns_persist_activation_projection_and_trace_inputs` + `fx_rust_real_cfg_builder_let_else_tuple_projection_persists_and_traces` + `n5_focus_rust_let_pattern_bindings_match_index_full` | ordinary `let`/`let-else` simple/nested capture 验证 enclosing-scope distinct identity、完整 declaration 末尾激活、initializer/alternative 的 outer identity、direct whole-value Assign(0.90)、fixed structural FieldLoad(0.80)→Assign(0.90)、post-rest aggregate Assign(0.75)、SQLite/Trace 与 Focus dataflow/binding/CFG==full Index；真实 `CfgBuilder::walk_labeled_statement` 锁定 `Some((label, body))` 的 `[0][0]`/`[0][1]`；borrow/move、runtime-length suffix 与 compiler irrefutability/type validation 保持显式边界 |
| Rust control-flow let pattern | `test_rust_control_let_bindings_preserve_scope_order_and_projection_flow` + `fx_rust_control_let_bindings_persist_scope_order_and_trace_exact_inputs` + `fx_rust_real_focus_engine_if_let_projection_persists_and_traces` + `n5_focus_rust_control_let_bindings_match_index_full` | `if let`、let-chain 与 `while let` 验证 source-ordered capture activation、later condition/success-body identity、`else`/after-loop exclusion、outer shadow restoration、fixed nested projection 的 FieldLoad(0.80)→Assign(0.90)、SQLite/Trace 与 Focus==full Index；真实 `FocusEngine` `if let Ok(symbols)` 锁定 call-result `[0]` 投影与 Trace；cold peer isolation、runtime-length suffix/borrow-move/condition dependency 边界保持显式 |
| Rust match guard CFG | `test_match_cfg_rust_guard_is_explicit_control_branch` + `test_match_cfg_rust_empty_guarded_arm_preserves_both_guard_outcomes` + `fx_cfg_real_rust_match_guard_persists_as_control_branch` + `n5_focus_rust_match_binding_dataflow_matches_index_full` | guarded arm 验证 dispatch `CaseBranch`→guard `Branch`、guard `TrueBranch`→body、guard `FalseBranch`→shared `Join`，空 body 仍保留两个 guard outcome；checked-in `FocusRuntime::strategies_for` 验证 SQLite persistence，synthetic let-chain guard 验证 Full Index/cold Focus whole-unit CFG parity 与 peer isolation；ordered pattern re-dispatch、pattern-predicate proof 与 guard-to-value variable Trace 保持显式边界 |
| FileLock 互斥 | `reject_if_held_by_foreign_live_pid` 等 | 其他 live PID → `cli_index_lock_held`；同 PID 豁免 |
| 短名 / 限定名解析 | `resolve_by_name_short_name_*`（atlas-engine） | 短名 `GetDev` 命中 `CertUtils::GetDev`；多短名 → Ambiguous + 全 qname `symbol_ref`；精确 qname 仍 UniqueQname |
| C++ 限定调用抽取 | `test_cpp_qualified_call_ref_simple_name_and_full_text`（extraction） | ref.name=`GetDev`、text=`CertUtils::GetDev`、receiver=`CertUtils`；嵌套 `A::B::method` 全文/前缀正确 |
| C++ 限定调用边 | `test_cpp_qualified_call_creates_calls_edge`（resolution） | call resolved + callers(`CertUtils::GetDev`) 含 `use_dev` |
| PHP 限定调用抽取 | `test_php_qualified_call_ref_simple_name_and_full_text`（extraction） | ref.name=`bar`；text/receiver 含 `Foo` |

发布前至少：`cargo test -p atlas-mcp --lib` 与 `cargo test -p atlas-engine --lib` 全绿。
| 单元正则 | `LinuxAugmenter` 直接测 | 无符号匹配、非 C 文件 no-op、syscall diagnostic 文案 |
| Structural DB 持久化 | extract + `Store::insert_file_facts` | DB 中 `exported=true` 且存在 `RegistersCallback` edge |
| ResolutionSymbols DB | extract + `upsert_resolution_symbols` | DB 中 `exported=true`；**无** initcall edge 行 |
| Index e2e | `atlas index --analysis structural` + C fixture | CLI index 路径与 extract 语义一致 |
| Lazy e2e | manifest index → `LazyStructuralService::ensure_structural_for_file` | lazy 路径同样持久化 export + initcall |

持久化分层：
- ResolutionSymbols 写路径只持久化 symbols/scopes/imports → **exported 标志**写入；initcall 边 / syscall diagnostics **不**进 DB。
- Structural 写路径写入 raw_edges → initcall 边可持久化。

改 hook 行为或移动挂载点时：必须更新本表，并跑：
```bash
cargo test -p extraction post_extract
cargo test -p atlas-cli --test lazy_index_e2e post_extract
```

### 2.11 语言能力展示（A1）

`atlas doctor` / `atlas status` / MCP `status` 对用户只反馈：
- 语言名
- `CapabilityLevel`（理论能力摘要）
- `confidence_floor`（语言置信度下限）

禁止在默认输出中展开完整 `FeatureMatrix` 或 “Unsupported Features” 明细列表。
`FeatureMatrix` 仍用于内部门控与单元测试；能力差异的可观察摘要是 confidence，而不是 feature 枚举。

### 2.12 清理与架构收敛 PR 门禁

清理类 PR 不能只证明“代码少了”，必须证明行为和架构边界没有漂移。

- 删除代码前确认零生产调用点、零测试支撑用途，或明确替代路径；测试 helper 不得按死代码处理。
- 抽取 helper/builder 时至少覆盖一个简单调用点和一个有分支的调用点。
- MCP 公共响应必须保持现行契约：`analysis`（含可空 `retry_after_ms`）、结构化 `gaps`、`query_id`；不得重新引入非公共字段进 JSON（见 architecture MCP 信封）。
- facade API 变更须有编译/测试验证，或在 CHANGELOG 标明 breaking。
- 至少：`cargo fmt --check`、`cargo check`、受影响 crate 测试；全量失败须在 review 中列明。

## 3. 现行能力测试要求

### 语言与 dataflow

全部默认语言须有 symbols/references/imports/calls golden 与 dataflow edge/path smoke（能力声明以 `FeatureMatrix` 为准）。

### Facade 与入口

- CLI/MCP 只调用 engine/API，不复制 resolver/graph/analysis 管线。
- engine / CLI / MCP 各有独立单元或集成/E2E；workspace feature 组合可编译。
- 高层 Engine 抽取只接受 project-relative `SourcePath`；Corpus-style caller-owned
  `FileId` 只走低层无 DB extraction API。

### Focus 公共 analysis 视图

- 触发 Focus materialize 的 MCP 工具经统一 `analysis` 暴露 scope、basis、summary 与可选 `retry_after_ms`。
- 终态缺口为公开 `{scope, reason, detail}`，不序列化内部枚举。
- `resume_query` / `tasks(query_id)` / Investigation 有对应回归。

### Domain Rules / Lifecycle

- `domain_rules` schema 与 registry 校验；disabled/candidate 等不参与匹配。
- C/C++ ownership 与 lifecycle/branch_diff 以 CFG/dataflow facts 为输入，不得手写最终 verdict 冒充 e2e。

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
