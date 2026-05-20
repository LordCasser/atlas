# Atlas 当前架构实现

本文描述当前代码已经落地的实现状态。未落地设计写在 [未来架构演进](./05-roadmap.md)。

## 1. 代码结构

```text
src/
  types/        ID、enum、IR、binding、dataflow、CFG、taint 类型
  db/           SQLite schema、store API、迁移入口
  extraction/   tree-sitter 解析、query、scope、semantic binder、lexical binder、dataflow、CFG
  resolution/   builtin filter、import/export/include/path alias/name matching
  graph/        GraphBuilder、GraphSnapshot、GraphEngine
  search/       FTS、LIKE/fuzzy、query parser、scoring
  context/      Agent context builder
  sync/         file discovery、change detection、file lock、watcher
  analysis/     taint analysis layer
  mcp/          MCP protocol、transport、tools
  cli/          CLI commands
```

`src/lib.rs` 当前声明的高层方向：

```text
CLI > MCP > Context/Graph/Search/Sync > Analysis > Resolution > Extraction > Database > Types
```

## 2. 当前数据流

```text
Source files
  -> discovery / file lock / worker
  -> tree-sitter parse
  -> query extraction through LanguageAdapter
  -> scope tree
  -> lexical binding
  -> local dataflow facts
  -> CFG facts
  -> SemanticBinder binds source_symbol, scope_id, binding
  -> Store writes FileFacts
  -> ReferenceResolver updates resolved_* fields
  -> GraphBuilder writes symbol_edges
  -> GraphSnapshot loads query graph
  -> CLI / MCP / Search / Context / Analysis
```

## 3. Schema 状态

当前 `CURRENT_SCHEMA_VERSION` 为 `7`。

主要表：

```text
files
symbols
scopes
references
imports
symbol_edges
callsites
bindings
binding_uses
data_nodes
dataflow_edges
callsite_args
cfg_nodes
cfg_edges
taint_rules
taint_findings
taint_path_steps
project_metadata
symbols_fts
schema_versions
```

关键状态：

- `references_v2` 已改为 SQL-quoted `"references"`。
- symbol-level edges 已使用 `symbol_edges`。
- dataflow facts 已使用 `data_nodes` 和 `dataflow_edges`。
- CFG facts 已使用 `cfg_nodes` 和 `cfg_edges`。
- taint rules/findings/path persistence 已进入 schema v7。

## 4. Extraction

当前抽取层包含：

- `engine.rs`
- `extract.rs`
- `grammar.rs`
- `scope_tree.rs`
- `symbol_registry.rs`
- `semantic_binder.rs`
- `lexical_binder.rs`
- `dataflow_builder.rs`
- `cfg_builder.rs`
- `worker.rs`
- `languages/`
- `queries/`

已落地能力：

- per-file extraction。
- `ParseWorkerPool`，支持 max file size、panic isolation、结构化 `ExtractionError` 和 `IndexReport`。
- `SemanticBinder` 统一填充 source/scope/binding 相关关系。
- `LexicalBinder` 和 `DataFlowBuilder` 已建立 P3 数据流基础。
- `CfgBuilder` 已建立函数级 CFG 基础。
- Golden test framework 已用于 TypeScript、Python、imports、C includes、CFG 等 fixtures。

已知限制：

- per-file timeout 尚未完全强制，受 adapter/threading 约束。
- 非 TS 语言的 lexical/dataflow 支持仍需要逐步补齐。
- CFG 不覆盖 try/catch/finally、switch/case、async/await、labeled break/continue。

## 5. Resolution 与 Graph

当前 resolution 层包含：

- `builtins.rs`
- `context.rs`
- `import_resolver.rs`
- `export_resolver.rs`
- `include_graph.rs`
- `path_alias.rs`
- `name_matcher.rs`

已落地能力：

- Resolver 与 GraphBuilder 分离。
- Resolver 返回 resolved facts 并更新 `"references"` 的 resolved fields。
- GraphBuilder 从 resolved references 创建 symbol-level edges。
- TS/JS path alias 支持 `tsconfig.json` 的 `paths/baseUrl`。
- ExportResolver 支持 re-export/barrel chain。
- IncludeGraph 支持 C/C++ local include、system include 过滤和 includer 查询。
- Sync 集成 resolved fact invalidation，删除/修改文件时清理相关 references/edges。

## 6. Search、Context、MCP、CLI

Search：

- 支持 FTS5、LIKE fallback、fuzzy matching。
- `SearchQueryParser` 支持 `kind:`、`lang:`、`path:`、`name:` 前缀。

Context：

- 基于 symbol、callers/callees、file peers、importers/dependencies 构建 Agent context。

MCP：

- JSON-RPC stdio。
- 工具集中在 `src/mcp/tools.rs`。
- 当前 README 中定义 12 个 Agent-facing 工具。

CLI：

- `init`
- `index`
- `sync`
- `search`
- `status`
- `files`
- `context`
- `mcp`
- `doctor`
- `taint`

## 7. Analysis / Taint

当前 `src/analysis/taint/` 已引入：

- `TaintRuleLoader`
- `TaintEngine`
- `TaintPathTracer`
- finding storage module

设计状态：

- 规则来自内置默认规则和 `.atlas/rules/*.yaml`。
- 默认规则覆盖 TypeScript/JavaScript 和 Python 的常见 source/sink/sanitizer。
- TaintEngine 基于 `DataNode` 和 `DataFlowEdge` 做 worklist forward propagation。
- TaintPathTracer 基于 reverse BFS 生成 source-to-sink path steps。
- 当前分析以 P3 dataflow 为基础，跨函数精度仍依赖后续函数摘要和更完整 interprocedural flow。

## 8. Cargo Features

默认 features：

```text
typescript
javascript
python
cli
```

MVP 语言 features：

```text
typescript
javascript
python
java
c
cpp
arkts
cangjie
```

未来语言 features 目前是 opt-in，不计入 MVP 验收。

## 9. 当前演进决策

当前阶段继续基于本文件描述的现有架构推进，不先拆分 workspace/crate，也不先开启 Corpus 分支。

当前主线目标：

1. 在现有 `types/db/extraction/resolution/graph/analysis/cli/mcp` 架构内完成污点分析。
2. 为所有 MVP 语言补齐 taint 所需 facts 和端到端测试。
3. 稳定 CLI/MCP 的 taint 查询输出。

完成上述目标后，再把核心能力拆成可复用 engine crate：

```text
engine: 语法解析 + facts + binding/dataflow/CFG + taint analysis
cli: command-line interaction
mcp: JSON-RPC transport and tools
```

拆分完成后，后续演进才分叉为 Atlas 单仓库单版本索引和 Corpus 多版本源码索引。
