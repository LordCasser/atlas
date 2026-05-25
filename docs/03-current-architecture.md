# Atlas 当前架构实现

本文描述当前代码已经落地的实现状态。当前和未来工作见 [Roadmap](./04-roadmap.md)。

## 1. 代码结构

项目当前是 13 个 Cargo package 的 workspace：根 `Cargo.toml` 包含 `crates/atlas-engine`、`crates/atlas-engine/crates/*`、`crates/atlas-cli`、`crates/atlas-mcp`。

```text
crates/
  atlas-engine/        facade crate，re-export types/db/extraction/resolution/graph/analysis/search/context/filesync
    crates/types/      ID、enum、IR、binding、dataflow、CFG、trace 查询类型
    crates/db/         SQLite schema、store API、readers、schema 初始化与迁移基础设施
    crates/workspace/  ProjectRoot、WorkspacePaths、SourcePath
    crates/extraction/ tree-sitter 解析、query、scope、semantic binder、lexical binder、dataflow、CFG
    crates/resolution/ builtin filter、scope/container/import/include/name matching；PathAliasResolver 已接入主路径
    crates/graph/      GraphBuilder、GraphSnapshot、GraphEngine
    crates/analysis/   变量来源追踪与调用路径查询分析层
    crates/search/     FTS、LIKE/fuzzy、query parser、scoring
    crates/context/    Agent context builder
    crates/filesync/   file discovery、change detection、file lock、watcher
  atlas-mcp/           MCP server adapter、protocol、tools
  atlas-cli/           CLI binary + commands + integration tests
```

依赖方向（严格无环）：

```text
atlas-cli → atlas-engine, atlas-mcp
atlas-mcp → atlas-engine
atlas-engine → types, workspace, db, extraction, resolution, graph, analysis, search, context, filesync
filesync → graph, resolution, extraction, db, types, workspace
search / context → graph, db, types
analysis → db, types, workspace
graph → db, types
resolution → db, types, workspace
extraction → types
db → types
workspace → (stdlib + anyhow)
types → (anyhow, blake3, hex, rusqlite, serde)
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

当前 `CURRENT_SCHEMA_VERSION` 为 `1`。schema 迁移基础设施已经落地：打开现有数据库时会检查 `schema_versions`，低版本可通过 `MIGRATIONS` 链升级，高版本会被拒绝。当前 `MIGRATIONS` 为空（V1），因此 V1 内 schema 变更仍应谨慎处理；发布前需要在 README 和 release notes 中明确兼容策略。

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
- `schema_versions` 记录 schema 历史；`atlas doctor` 可报告当前版本、过新版本和缺失迁移路径。

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
- Java/C/C++/ArkTS/Go/Rust/C#/PHP/Ruby/Kotlin 的 capability profile 当前声明 `DataflowBasic`，但仍有明显语言级 limitation；完整 path-level 变量来源追踪、CFG 和跨函数传播仍需逐语言 fixture 约束。
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

- 基于官方 Rust SDK `rmcp` 的 stdio transport。
- 工具按能力分类组织在 `crates/atlas-mcp/src/tools/` 目录。
- 当前 MCP 注册 23 个 Agent-facing 短名工具：`open_project`、`index`、`status`、`files`、`search`、`symbol`、`neighbors`、`callers`、`callees`、`callgraph`、`path`、`explore`、`impact`、`context`、`trace_point`、`trace_variable`、`trace_caller_path`、`language_capabilities`、`usages`、`dependencies`、`dependents`、`task_status`、`wait_for_task`。当前公开契约使用无 `atlas_` 前缀的短名；`task_status`/`wait_for_task` 用于消费 `background=true` 返回的后台任务。

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
- `trace`


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
- 当前 capability 边界：TypeScript/JavaScript/Python/Java/C/C++/ArkTS/Go/C#/Rust/PHP/Ruby/Kotlin 为 `DataflowBasic`，但除 TS/JS/Python 外主要是基础局部 dataflow + explicit limitations；Bash/Cangjie 是显式 opt-in experimental，其中 Bash 为 Symbolic、Cangjie 为 Symbolic 且 call graph/dataflow 仍 unsupported。`all-languages` 包含 MVP 7 语言和 Go/C#/Rust/PHP/Ruby/Kotlin，不包含 Bash/Cangjie。

## 8. Cargo Features

默认 features：

```text
typescript
javascript
python
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

Post-MVP DataflowBasic features 已接入 `all-languages`，但不等同于完整 trace 生产验收：

```text
go
csharp
rust
php
ruby
kotlin
```

不完善/实验语言 features 目前是 opt-in，不计入 MVP 验收：
- Cangjie（Symbolic）已纳入 `all-languages`（自 tree-sitter 0.26 ABI 兼容）
- Bash 不在 `all-languages`，需显式启用 `bash` feature

```text
bash
```

未来新增语言仍按独立 adapter/query/fixture/capability profile 接入，不得修改中心 mega-extractor。

## 9. 当前演进决策

**Item 10 (workspace/crate 拆分) 已完成。** 项目已从单 crate 演进为 `atlas-engine` facade、engine 内部 crates、`atlas-mcp` 和 `atlas-cli` 组成的 workspace，并通过 facade 约束上层入口依赖。

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
2. 为所有 `DataflowBasic` 语言按能力等级补齐 trace 所需 facts 和端到端测试；post-MVP 语言先锁定 capability、golden fixtures 和 dataflow smoke，不把基础 dataflow 直接宣称为完整变量来源追踪。
3. 稳定 CLI/MCP 的 trace 查询输出。

完成 Item 10 拆分后，crate 边界已建立：

```text
atlas-cli: CLI binary + commands (编排所有能力)
atlas-mcp: JSON-RPC transport and tools
atlas-engine: facade public API and re-exports
atlas-engine/crates/filesync: 增量索引引擎 (file discovery, hash detection, watcher)
atlas-engine/crates/search/context: 查询和 AI 上下文构建
atlas-engine/crates/analysis: 变量来源追踪与调用路径查询引擎
atlas-engine/crates/graph/resolution: 图构建与符号解析
atlas-engine/crates/extraction: 语法解析 + facts + binding/dataflow/CFG
atlas-engine/crates/db: SQLite 持久化层
atlas-engine/crates/types: 核心类型系统
atlas-engine/crates/workspace: 项目根目录与路径抽象
```

后续演进可在此边界上分叉为 Atlas 单仓库单版本索引和 Corpus 多版本源码索引。

## 10. 相关文档

- 架构约束与不变式：见 [`02-architecture-constraints.md`](./02-architecture-constraints.md)
- 语言能力权威表（从代码 capability profile 导出）：

| Language | Level | CFG | Confidence | In all-languages? |
|----------|-------|:---:|:---:|:---:|
| TypeScript | DataflowBasic | ✓ | 0.55 | ✓ (default) |
| JavaScript | DataflowBasic | ✓ | 0.55 | ✓ (default) |
| Python | DataflowBasic | ✗ | 0.50 | ✓ (default) |
| Java | DataflowBasic | ✗ | 0.65 | ✓ |
| C | DataflowBasic | ✗ | 0.65 | ✓ |
| C++ | DataflowBasic | ✗ | 0.60 | ✓ |
| ArkTS | DataflowBasic | ✗ | 0.45 | ✓ |
| Go | DataflowBasic | ✗ | 0.70 | ✓ |
| C# | DataflowBasic | ✗ | 0.70 | ✓ |
| Rust | DataflowBasic | ✗ | 0.60 | ✓ |
| PHP | DataflowBasic | ✗ | 0.55 | ✓ |
| Ruby | DataflowBasic | ✗ | 0.50 | ✓ |
| Kotlin | DataflowBasic | ✗ | 0.65 | ✓ |
| Cangjie | Symbolic | ✗ | 0.60 | ✓ |
| Bash | Symbolic | ✗ | 0.40 | ✗ (opt-in) |

- Lazy dataflow 设计：analysis 层按需加载 dataflow facts（而非全量预加载），通过 `LazyWindow` 控制分析范围，`ExtractionMode::LazyDataflow` 支持增量按需抽取。详见 `dataflow_builder.rs` 和 `extraction_ctx.rs`。
