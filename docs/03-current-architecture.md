# Atlas 当前架构实现

本文描述当前代码已经落地的实现状态。未落地设计写在 [未来架构演进](./05-roadmap.md)。

## 1. 代码结构

项目已拆分为 Cargo workspace（12 crates），根 `Cargo.toml` 为纯 workspace (members = `["crates/*"]`)。

```text
crates/
  atlas-types/        ID、enum、IR、binding、dataflow、CFG、trace 查询类型
  atlas-db/           SQLite schema、store API、readers、schema 初始化
  atlas-workspace/    ProjectRoot、WorkspacePaths、SourcePath
  atlas-extraction/   tree-sitter 解析、query、scope、semantic binder、lexical binder、dataflow、CFG
  atlas-resolution/   builtin filter、scope/container/import/include/name matching；PathAliasResolver 已接入主路径
  atlas-graph/        GraphBuilder、GraphSnapshot、GraphEngine
  atlas-analysis/     变量来源追踪与调用路径查询分析层
  atlas-search/       FTS、LIKE/fuzzy、query parser、scoring
  atlas-context/      Agent context builder
  atlas-sync/         file discovery、change detection、file lock、watcher
  atlas-mcp/          MCP protocol、transport、tools
  atlas-cli/          CLI binary + commands + all tests
```

依赖方向（严格无环）：

```text
atlas-cli → atlas-mcp, atlas-sync, atlas-search, atlas-context, atlas-analysis, atlas-graph,
            atlas-resolution, atlas-extraction, atlas-db, atlas-types, atlas-workspace
atlas-mcp → atlas-context, atlas-search, atlas-graph, atlas-analysis, atlas-db, atlas-types, atlas-workspace
atlas-sync → atlas-graph, atlas-resolution, atlas-extraction, atlas-db, atlas-types
atlas-search / atlas-context → atlas-graph, atlas-db, atlas-types
atlas-analysis → atlas-db, atlas-types, atlas-workspace
atlas-graph / atlas-resolution → atlas-db, atlas-types
atlas-extraction → atlas-types
atlas-db → atlas-types
atlas-workspace / atlas-types → (stdlib only)
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
- `dataflow_edges` 已持久化完整 6 字段 TextRange（包含 start/end line+column），trace evidence 精确 line/column 往返可用。

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
- `include_graph.rs`
- `path_alias.rs`
- `name_matcher.rs`

已落地能力：

- Resolver 与 GraphBuilder 分离。
- Resolver 返回 resolved facts 并更新 `"references"` 的 resolved fields。
- GraphBuilder 从 resolved references 创建 symbol-level edges。
- `PathAliasResolver` 已接入主解析路径（index 和 sync 在项目根存在 tsconfig.json 时自动加载 path aliases）。
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
- 工具按能力分类组织在 `src/mcp/tools/` 目录。
- 当前 MCP 注册 16 个 Agent-facing 工具：status、files、search、symbol、neighbors、callers、callees、callgraph、path、explore、impact、context、trace_point、trace_variable、trace_caller_path、language_capabilities。

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

当前架构已经落地显式语言能力边界，但仍需要持续校准事实精度和文档宣称：

- analysis/types 层提供 capability profile / feature matrix，描述每种语言当前支持的 trace level、supported features、unsupported features、known limitations 和 confidence floor。
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

**Item 10 (workspace/crate 拆分) 已完成。** 项目已从单 crate 拆分为 12 个 Cargo workspace crate，
严格遵循 types → db → workspace → extraction → resolution → graph → analysis → search → context → sync → mcp → cli 的依赖方向。

已完成演进（P6: 索引性能优化）：

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

完成 Item 10 拆分后，crate 边界已建立：

```text
atlas-cli: CLI binary + commands (编排所有能力)
atlas-mcp: JSON-RPC transport and tools
atlas-sync: 增量索引引擎 (file discovery, hash detection, watcher)
atlas-search/context: 查询和 AI 上下文构建
atlas-analysis: 变量来源追踪与调用路径查询引擎
atlas-graph/resolution: 图构建与符号解析
atlas-extraction: 语法解析 + facts + binding/dataflow/CFG
atlas-db: SQLite 持久化层
atlas-types: 核心类型系统
atlas-workspace: 项目根目录与路径抽象
```

后续演进可在此边界上分叉为 Atlas 单仓库单版本索引和 Corpus 多版本源码索引。
