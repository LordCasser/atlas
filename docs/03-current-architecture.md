# Atlas 当前架构实现

本文描述当前代码已经落地的实现状态。未落地设计写在 [未来架构演进](./05-roadmap.md)。

## 1. 代码结构

```text
src/
  types/        ID、enum、IR、binding、dataflow、CFG、trace 查询类型
  db/           SQLite schema、store API、schema 初始化
  extraction/   tree-sitter 解析、query、scope、semantic binder、lexical binder、dataflow、CFG
  resolution/   builtin filter、import/export/include/path alias/name matching
  graph/        GraphBuilder、GraphSnapshot、GraphEngine
  search/       FTS、LIKE/fuzzy、query parser、scoring
  context/      Agent context builder
  sync/         file discovery、change detection、file lock、watcher
  analysis/     变量来源追踪与调用路径查询分析层
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
  -> CLI / MCP / Search / Context / Analysis / Trace
```

## 3. Schema 状态

当前 `CURRENT_SCHEMA_VERSION` 为 `1`。项目仍处于快速开发阶段，当前不维护部署迁移或旧库兼容承诺。

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
callsite_args (deprecated table; call arguments are currently stored inline on callsites)
cfg_nodes
cfg_edges
project_metadata
symbols_fts
schema_versions
```

关键状态：

- `references_v2` 已改为 SQL-quoted `"references"`。
- symbol-level edges 已使用 `symbol_edges`。
- dataflow facts 已使用 `data_nodes` 和 `dataflow_edges`。
- CFG facts 已使用 `cfg_nodes` 和 `cfg_edges`。
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


## 7. Analysis / Trace

Atlas 不包含污点分析（taint analysis）。当前产品主线为变量来源追踪与调用路径查询：

```text
用户指定位置 / callsite / 问题变量
  -> 定位 DataNode / BindingUse / ReferenceUse
  -> backward slice 追踪变量来源
  -> 结合 callers/callees 找到可能调用路径
  -> 输出 bounded evidence 给 Agent 分析
```

该能力不依赖内置漏洞规则系统，也不做全项目自动 finding。外部工具或用户可以把疑似问题点作为入口传给 Atlas，但 Atlas 只返回程序结构证据，不负责判定漏洞。

当前架构需要补齐显式语言能力边界：

- analysis 层应提供 `LanguageCapabilityProfile` 或等价结构，描述每种语言当前支持的 trace level、supported features、unsupported features、known limitations 和 confidence floor。
- CLI/MCP/context 不能自行推断语言能力，只能展示 analysis/engine 返回的 capability。
- trace 查询即使返回 partial result，也必须同时返回 capability 和 diagnostics，说明哪些路径是完整证据、哪些只是 best-effort、哪些请求超出当前语言能力。
- 当前默认边界：TypeScript/JavaScript/Python 作为 Level 3 主目标推进；Java/C/C++/ArkTS 以 Level 1/2 best-effort 输出；Cangjie 不属于默认或 `all-languages` 编译，仅在显式启用 `cangjie` feature 时作为 experimental minimal support。

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
```

不完善/实验语言 features 目前是 opt-in，不计入 MVP 验收：

```text
cangjie
```

未来语言 features 目前也是 opt-in，不计入 MVP 验收。

## 9. 当前演进决策

当前阶段继续基于本文件描述的现有架构推进，不先拆分 workspace/crate，也不先开启 Corpus 分支。

已完成增强（P6: 索引性能优化）：

- **P0**: 阶段耗时与语言级统计 (`PhaseTimings` / `PerLanguageStats`)
- **P1**: Hash-based 脏文件集增量索引 (dirty set, 干净文件跳过)
- **P2**: Thread-local Parser + LanguageFrontend 缓存
- **P3**: 批量事务 DB 写入 (`insert_file_facts_batch`)
- **P4**: 全局内存符号索引 (`GlobalSymbolIndex`) + in-memory resolution
- **P5**: Graph edge 并行构建 (Rayon)
- **P6**: Dataflow/CFG 按需加载 (trace 查询已按需)
- **P7**: 语言能力驱动的 extraction 跳过策略

当前主线目标：

1. 在现有 `types/db/extraction/resolution/graph/analysis/cli/mcp` 架构内完成变量来源追踪与调用路径查询。
2. 为 MVP 语言按能力等级补齐 trace 所需 facts 和端到端测试。
3. 稳定 CLI/MCP 的 trace 查询输出。

完成上述目标后，再把核心能力拆成可复用 engine crate：

```text
engine: 语法解析 + facts + binding/dataflow/CFG + variable provenance trace / caller path query
cli: command-line interaction
mcp: JSON-RPC transport and tools
```

拆分完成后，后续演进才分叉为 Atlas 单仓库单版本索引和 Corpus 多版本源码索引。
