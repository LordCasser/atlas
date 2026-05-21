# 阶段日志

本文记录 P0-P5 阶段实施摘要。方向性变更说明见 [架构与需求变更说明](./04-changes.md)。

## P0：语义绑定与引用冲突修复

目标：修复基础事实层的 ID 冲突和 source/scope 绑定职责混乱。

完成内容：

- `ReferenceId::generate()` 加入 `ReferenceKind`，修复同 range 的 call/field captures 互相覆盖。
- `references_v2` 重命名为 SQL-quoted `"references"`。
- 新增 `SemanticBinder`，统一填充 `source_symbol` 和 `scope_id`。
- Adapter 不再手动查找 enclosing function。
- 修复 TSX/JSX 和 ArkTS `.sts` 等语言 metadata。

保留不变式：

- references 永不因 resolved 而删除。
- ID 使用 deterministic blake3。
- source/scope 由 SemanticBinder 统一处理。
- semantic facts 必须携带 confidence/provenance。

## P1：产品化索引基础

目标：让大项目索引失败可控，Agent 和 CLI 能看到清晰报告。

完成内容：

- 新增 `FailureCategory`、`ExtractionError`、`IndexReport`、`WorkerConfig`。
- 新增 `ParseWorkerPool`，支持 panic isolation、max file size 和结构化错误。
- 新增 `SearchQueryParser`，支持 `kind:`、`lang:`、`path:`、`name:`。
- 新增 SQLite-based `FileLock`。
- 建立 golden test framework。

结果：

- 单文件失败不应中断整个项目索引。
- 索引报告能展示 discovered、indexed、skipped、failed、resolution rate 等信息。

## P2：Resolver 与 GraphBuilder 分离

目标：让 resolution、edge 构建和增量失效职责清晰。

完成内容：

- `ReferenceResolver.resolve_all()` 改为只返回 resolved facts。
- 新增 `GraphBuilder`，负责创建 symbol-level edges。
- 新增 `invalidate_references_for_file` 和 `delete_edges_for_file_references`。
- 新增 `PathAliasResolver`，支持 TS/JS `tsconfig.json` paths/baseUrl。
- 新增 `ExportResolver`，支持 re-export/barrel chain。
- 新增 `IncludeGraph`，支持 C/C++ local include 和 system include 过滤。
- Sync 和 CLI 改为 Resolver -> GraphBuilder 两步流程。

结果：

- 跨文件调用图更稳定。
- 文件删除或修改后可以清理悬空 resolved target 和旧 edges。

## P3：Binding 与 DataFlow 基础

目标：为变量来源追踪与调用路径查询建立局部绑定和数据流事实，不再把 dataflow 塞进 symbol graph。

完成内容：

- 新增 `BindingId`、`BindingUseId`、`DataNodeId`、`DataFlowEdgeId`。
- 新增 `BindingDef`、`BindingUse`、`DataNode`、`DataFlowEdge`、`CallsiteArg`。
- `"references"` 增加 `binding_id`。
- `edges` 拆为 `symbol_edges`。
- 新增 `bindings`、`binding_uses`、`data_nodes`、`dataflow_edges`、`callsite_args`。
- 新增 `LexicalBinder` 和 `DataFlowBuilder`。

核心结论：

- dataflow 必须是 `DataNodeId -> DataNodeId`。
- 禁止用 fake `SymbolId` 表达变量、表达式、参数或返回值的数据流。

## P4：CFG 基础

目标：为后续精度增强提供函数内控制流骨架。CFG 不作为 P5 变量来源追踪与调用路径查询 MVP 的前置门槛。

完成内容：

- 新增 `CfgNodeId`、`CfgEdgeId`。
- 新增 `CfgNodeKind` 和 `CfgEdgeKind`。
- 新增 `cfg_nodes` 和 `cfg_edges`。
- 新增 `CfgBuilder`，为函数构建 Entry/Exit、Statement、Branch、Loop、Return、Throw、Join 等节点。
- `extract_file()` 集成 CFG 构建。
- Golden fixtures 扩展 CFG 期望。

明确推迟：

- `FunctionSummary`。
- `IntraproceduralDataflow` / `InterproceduralDataflow` 专用抽象。
- `BindingGraph` / `DataFlowGraph` 的专用 in-memory graph。

原因：当前消费者不足，P5 trace 可以先从 DB facts 按需加载。

## P5：Trace 查询原型

目标：在 P3/P4 基础上验证指定位置驱动的变量来源追踪与调用路径查询。当前 schema 不包含自动扫描规则、finding 或扫描路径步骤表；Atlas 不包含污点分析（taint analysis）。

完成内容：

- 新增 MCP 工具 `atlas_trace_point`、`atlas_trace_variable`、`atlas_trace_caller_path`。
- CLI 新增 `atlas trace` 子命令 — 变量来源追踪与调用路径查询。

明确推迟：
- 函数摘要 (FunctionSummary) — 跨函数传播当前硬依赖 dataflow_edges。

明确不做：

- 跨语言自动扫描规则生态。

测试覆盖：

- Path tracer 单元测试。
- E2E 集成测试 — 用预制 DataNode + DataFlowEdge 模拟数据流查询场景。

方向调整：

- 不再把"全项目自动 finding"作为当前主线验收。
- 当前主线改为用户指定位置、变量或调用点后的变量来源追踪与调用路径查询。

## Post-P5：工程成熟度改进

### 问题修复

- **MCP 编译修复**：trace 相关工具在 `--features mcp` 下编译失败，修复 import 和类型匹配。
- **ParseWorkerPool 接入 index 命令**：原来的 index 命令手动管理 `IndexFailure` 和并行逻辑。统一为由 `ParseWorkerPool.extract_one()` 驱动，`IndexReport` 替换 `IndexFailure`。
- **DataNode function_id 填充**：DataFlowBuilder 产出的 DataNode 原来 `function_id: None`。新增 `resolve_dataflow_function_ids()` 在 extraction 阶段按 range 匹配 function symbol，填充 `function_id`。
- **删除旧 normalize_dataflow → RawEdge 路径**：LanguageAdapter trait 中移除 `dataflow_query()` 和 `normalize_dataflow()`。RawEdge 仍保留（GraphBuilder 用于 structural edges），但 dataflow edge 只通过 DataNode→DataNode 路径生成。删除 6 个 `.scm` query 文件，更新所有 8 个 adapter。

### DataFlowBuilder 增强

- **use-def 跨语句边**：新增 `DataFlowBuilder::resolve_use_def()`，按 `(function_id, 小写变量名)` 分组 DataNodes，在各组内从第一个 Local/Parameter（定义）创建 Assign 边到后续 Expr/CallArg/Field/Return（使用）（confidence 0.85）。保守启发式算法，非 SSA 精度。集成到 extraction 流程 step 7e。
- 新增 3 个测试：unit test（预制 DataNodes）、真实 TS 提取 test、跨语句变量传递 test。
- 当前状态：DataFlowBuilder 已补充函数参数、call target、完整 access_path 和 access_path_pattern 匹配；变量来源追踪与调用实参查询还需要真实源码端到端 fixture 继续约束精度。

## P6：索引性能优化

目标：将 index pipeline 设计为分阶段、可缓存、可并行、单写入器批量落库的性能最优形态。

### P0：阶段耗时与语言统计

- 新增 `PhaseTiming`、`PhaseTimings`、`PerLanguageStats` 类型（`src/types/timing.rs`）。
- `IndexReport` 新增 `phase_timings` 和 `per_language` 字段。
- `atlas index` 和 `atlas sync` 均输出对齐的阶段耗时表和语言级统计。
- 输出示例：
  ```
  Discovery:        12ms,  2 items
  Hash check:         3ms,  2 items, 0 reused, 2 dirty
  Parse/extract:    108ms,  2 items, avg 54ms/file
  DB write:           2ms,  2 items
  Resolution:         0ms,  3 items, 1 resolved
  Graph build:        0ms,  0 items
  ```

### P1：Hash-based 脏文件集增量索引

- `atlas index` 新增 `Hash check` 阶段：并行计算文件 blake3 hash，与 DB 中已有 hash 对比。
- 未变化的文件跳过 extraction；只对新文件和内容变化的文件重新提取。
- 已删除文件（DB 中有记录但磁盘上不存在）自动清理相关 facts 和 edges。
- 第二次 index 从 130ms 降到 12ms（91% 减少），仅剩 hash 比较和空 resolution。

### P2：LanguageFrontend / parser 缓存

- 新增 thread-local `tree_sitter::Parser`（`src/extraction/extract.rs`）：每个 Rayon worker 线程复用同一个 parser 实例，避免 per-file `Parser::new()` 分配。
- `atlas index` 在 Language init 阶段预构建 `HashMap<Language, LanguageFrontend>` 缓存，所有文件共享同一批 frontend 实例，不再 per-file 调用 `create_frontend()`。

### P3：单写入器批量事务 DB 写

- `Store` 新增 `insert_file_facts_batch()` 方法：多文件在同一个 SQLite 事务中写入。
- `insert_file_facts()` 内部委托给共享实现 `insert_file_facts_impl()`。
- `atlas index` 按 200 文件一批调用 batch insert，大幅减少 transaction 开销。

### P4：Resolution 内存索引 + 并行 resolve

- 新增 `GlobalSymbolIndex`（`src/resolution/context.rs`）：一次性加载全部 symbols 到内存，构建 name→symbols 和 id→symbol 索引。
- Strategy 6（项目级名称搜索）从 per-reference FTS5 查询改为 in-memory HashMap 查找 + bounded Levenshtein fuzzy fallback。
- `ReferenceResolver` 增加 `global_index: Option<GlobalSymbolIndex>`，`resolve_all()` 首次调用时构建全局索引。

### P5：Graph 边并行构建

- `GraphBuilder::build_all()` 改为 Rayon `par_iter()` 并行创建边。
- 每个 reference 的边创建独立无依赖，适合完全并行化。
- 警告通过 `Mutex<Vec<String>>` 线程安全收集。

### P6：Dataflow/CFG 按需加载

- Trace 查询已按需从 DB 加载 dataflow edges（`dataflow_edges(target)` 反向索引），不做全量预加载。
- `GraphSnapshot` 只负责 symbol-level graph，dataflow/CFG 不在 snapshot 中。

### P7：语言能力驱动的跳过策略

- `extract_file()` 在构建 lexical bindings、dataflow 和 CFG 前检查 `frontend` 的 capability。
- 能力不支持的步骤直接跳过（返回空 vec），避免运行无意义的 tree-sitter query。
- 受影响语言：
  - Python：跳过 lexical（unsupported），保留 dataflow（supported with limitations）
  - Java：跳过 lexical + dataflow（均 unsupported）
  - C/C++：跳过 lexical + dataflow（均 unsupported）
  - Cangjie：跳过 lexical + dataflow + CFG（minimal only）
- Golden fixtures 已重生成以匹配新行为。

### 测试矩阵

所有 feature 组合通过：
- `cargo test` — 297 passed
- `cargo test --features "all-languages"` — 328 passed
- `cargo test --features "mcp"` — 318 passed
- `cargo test --features "all-languages,mcp"` — 350 passed
