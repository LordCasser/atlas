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

目标：为污点分析建立局部绑定和数据流事实，不再把 dataflow 塞进 symbol graph。

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

目标：为跨过程 dataflow 和 taint 提供函数内控制流骨架。

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

原因：当前消费者不足，P5 taint 可以先从 DB facts 按需加载。

## P5：Taint 分析 MVP

目标：在 P3/P4 基础上提供 source-to-sink 分析雏形。

完成内容：

- schema v7 新增 `taint_rules`、`taint_findings`、`taint_path_steps`。
- 新增 `src/types/taint.rs` — `TaintFindingId`、`TaintRule`、`TaintFinding`、`TaintPathStep`、`Severity`、`TaintRuleKind`。
- 新增 `src/analysis/taint/` 模块：
  - `rules.rs` — 内嵌 TS/Python 默认规则 + YAML 用户规则加载 (覆盖逻辑: language+kind+callee+symbol_pattern 匹配)。
  - `engine.rs` — forward propagation via worklist, source/sink/sanitizer matching, max_depth=20。
  - `path.rs` — reverse BFS path tracer, 从 sink 回溯 source 并构建 forward steps。
- 新增 MCP 工具 `atlas_taint_findings` 和 `atlas_taint_path`。
- CLI 新增 `atlas taint` 子命令 — 加载规则 → 数据流 → engine → path trace → 持久化。
- Store 新增 `get_data_node()` 单节点查询、taint 表读写 API。

默认规则覆盖：
- TypeScript: 9 sources (req.query, req.body, req.params 等), 11 sinks (exec, eval, innerHTML 等), 4 sanitizers.
- Python: 6 sources (request.args, request.form 等), 8 sinks (os.system, subprocess.call 等), 3 sanitizers.

明确推迟：
- 函数摘要 (FunctionSummary) — 跨函数传播当前硬依赖 dataflow_edges。
- 跨语言规则共享和规则生态。
- Path slicing 和 sanitizer 链精细化。
