# Atlas MVP 实施计划

> 当前有效实施计划。此计划替代早期 bottom-up CodeGraph parity 迁移计划。

---

## 1. 实施策略

Atlas MVP 使用 **vertical slices + language fixtures** 推进，而不是先完整迁移所有模块再接语言。

核心原则：

1. 先打通 `parse -> facts -> store -> resolve -> graph -> MCP` 闭环。
2. 每个阶段都必须有 fixture 和端到端验收。
3. MVP 语言只做 C / C++ / Python / Java / ArkTS / TypeScript / JavaScript / Cangjie。
4. C/C++ 明确 best-effort，Cangjie 先 grammar spike。
5. 不实现大型 GenericExtractor。

---

## 2. 里程碑总览

| Milestone | 名称 | 目标 |
|---|---|---|
| M0 | Foundations | IR、IDs、schema、grammar spike、AST dump | ✅ 2026-05-18 |
| M1 | Store & CLI skeleton | `.atlas` DB、migrations、基础 CLI (init/status/doctor/index) | ✅ 2026-05-18 |
| M2 | Query Extraction | tree-sitter query engine + TS/Python LanguageAdapters | ✅ 2026-05-18 |
| M3 | Extraction Pipeline | QueryEngine + extract_file() + insert_file_facts() end-to-end | ✅ 2026-05-18 |
| M4 | Resolution | scope/import/include/name resolution (6-stage pipeline) | ✅ 2026-05-18 |
| M5 | GraphSnapshot | 内存图与图查询 | 🚧 Next |
| M6 | Search & Context | FTS/hybrid search/context/explore |
| M7 | MCP MVP | MCP tools 可供 Agent 使用 |
| M8 | Incremental Sync | 增量同步和 snapshot refresh |
| M9 | Dataflow-lite Foundation | callsite/argument/return/assignment 基础 |

---

## 3. M0: Foundations

### 目标

建立不可逆的核心数据模型，避免后续被旧 CodeGraph schema 绑架。

### 任务

1. 定义 core IDs：

```text
FileId
SymbolId
ScopeId
ReferenceId
EdgeId
CallsiteId
OccurrenceId
```

2. 定义 core IR：

```text
FileFacts
SymbolDef
ScopeDef
ReferenceUse
ImportDef
ExportDef
RawEdge
Callsite
TextRange
```

3. 定义 enums：

```text
Language
SymbolKind
ReferenceKind
EdgeKind
ImportKind
ResolutionStrategy
Provenance
Visibility
```

4. 设计 SQLite schema：

```text
files
symbols
scopes
references
imports
edges
callsites
project_metadata
schema_migrations
symbols_fts
```

5. AST dump 工具：

```text
atlas ast-dump <file> # 可以先 hidden/dev command
```

6. MVP grammar spike：

```text
tree-sitter-c
tree-sitter-cpp
tree-sitter-python
tree-sitter-java
tree-sitter-typescript
tree-sitter-cangjie
```

重点验证 Cangjie。

### 验收

- `cargo test core` 通过。
- 能创建 `.atlas/atlas.db` 并执行 schema migration。
- AST dump 可输出 TS/Python/Java/C/C++/Cangjie fixture 的 AST。
- Cangjie grammar 状态有明确结论：可用、需 patch、或 fallback 方案。

---

## 4. M1: Store & CLI skeleton

### 目标

建立持久化和基础命令。

### 任务

1. `atlas init` 创建 `.atlas`。
2. `atlas status` 显示 DB/schema/language support。
3. `atlas doctor` 检查：

```text
SQLite FTS5
grammar availability
Cangjie grammar
project root
schema version
```

4. Store writer：batch insert files/symbols/scopes/references/imports/edges/callsites。
5. Store reader：basic query by id/name/file。
6. Migration system。

### 验收

- `atlas init && atlas status` 正常。
- SQLite schema 可重复初始化。
- 插入/读取 mock FileFacts 通过测试。

---

## 5. M2: Query Extraction

### 目标

实现 `tree-sitter queries + LanguageAdapter` 抽取体系。

### 任务

1. `LanguageAdapter` trait。
2. `GrammarRegistry`。
3. `QueryEngine`。
4. `ScopeBuilder`。
5. MVP adapters：

```text
TypeScriptAdapter
JavaScriptAdapter
ArkTSAdapter
PythonAdapter
JavaAdapter
CAdapter
CppAdapter
CangjieAdapter minimal
```

6. Query files：

```text
definitions.scm
references.scm
imports.scm
scopes.scm optional
```

7. `atlas index --no-resolve` 写入 raw facts。

### 语言最低抽取

- TS/JS/ArkTS：function/class/method/import/export/call/new。
- Python：function/class/method/import/call/decorator。
- Java：package/import/class/interface/method/constructor/call/new/extends/implements。
- C：function/struct/enum/include/call。
- C++：namespace/class/struct/method/include/call/new/extends。
- Cangjie：package/import/function/class-or-struct/method/call。

### 验收

每种语言 fixtures 至少验证：

```text
symbols count
contains edges
references count
imports/includes count
callsites count
line/byte ranges
```

---

## 6. M3: Resolution

### 目标

将 references 解析为 edges，同时保留 references。

### 任务

1. Builtin/external filters。
2. Scope-local exact resolver。
3. Container/class-local resolver。
4. Same-file resolver。
5. Import/include resolver：

```text
TS/JS/ArkTS relative/named/default/namespace/re-export basic
Python import/from/relative basic
Java package/import/wildcard/same package
C/C++ local include/system include filter
Cangjie import/same module
```

6. Name matcher with proximity scoring。
7. Confidence model。
8. Edge promotion：

```text
calls -> instantiates if target is class/struct
extends -> implements if target is interface/trait/protocol-like
```

### 验收

- 调用边可生成。
- import/include 边可生成。
- extends/implements 边可生成。
- references 表保留 resolved status。
- 低置信度解析不会冒充高置信度。

---

## 7. M4: GraphSnapshot

### 目标

低延迟图查询。

### 任务

1. 从 SQLite 加载 nodes/edges/references summary。
2. 建立：

```text
id_to_idx
name_index
qname_index
file_index
outgoing adjacency
incoming adjacency
```

3. 实现：

```text
neighbors
callers
callees
callgraph
impact
shortest_path
usages
file dependencies
```

4. 支持 confidence filter。

### 验收

- 图查询不在每一步 hit SQLite。
- BFS/DFS/path 有 depth/limit 防护。
- impact 能展开容器 children。

---

## 8. M5: Search & Context

### 目标

让 LLM Agent 能快速定位相关代码。

### 任务

1. FTS5 search。
2. exact / qname / LIKE / CamelCase / fuzzy search。
3. 多信号 scoring：

```text
name match
kind bonus
path relevance
language filter
test downrank
multi-term boost
```

4. Context pipeline：

```text
extract query terms
search entry points
resolve imports to definitions
expand type hierarchy
traverse graph
per-file diversity cap
extract code blocks
format markdown/json
```

5. Explore pipeline：

```text
group by file
relationship map
merge source ranges
additional files list
output budget
```

### 验收

- `atlas search auth` 返回相关符号。
- context/explore 在 fixtures 上返回 entry points、relationships、code snippets。
- 输出有上限，不无限膨胀。

---

## 9. M6: MCP MVP

### 目标

LLM Agent 可通过 MCP 使用 Atlas。

### 工具

```text
atlas_status
atlas_files
atlas_search
atlas_symbol
atlas_neighbors
atlas_callers
atlas_callees
atlas_callgraph
atlas_impact
atlas_path
atlas_context
atlas_explore
```

### 任务

1. JSON-RPC stdio transport。
2. MCP initialize/tools/list/tools/call。
3. projectPath 支持。
4. Tool input validation。
5. Output budget / pagination / truncation。
6. Error formatting。

### 验收

- MCP client 能 list tools。
- 每个工具有 fixture-based integration test 或协议测试。
- 未初始化项目给出清晰错误。

---

## 10. M7: Incremental Sync

### 目标

项目变化后快速更新图谱。

### 任务

1. `git status --porcelain` detector。
2. mtime/hash fallback。
3. 删除 removed/changed file facts。
4. 重新 parse changed files。
5. affected refs re-resolution。
6. GraphSnapshot reload。
7. Optional watcher。

### 验收

- 修改一个文件后仅该文件重建。
- 删除文件后 symbols/edges/references 清理。
- 新增调用后 callers/callees 查询更新。

---

## 11. M8: Dataflow-lite Foundation

### 目标

为未来污点分析打基础，不要求完整 taint。

### 任务

1. Callsite arguments extraction where feasible。
2. Function parameters。
3. Return expressions basic。
4. Assignment facts basic。
5. Internal edges：

```text
argument
parameter
returns
assigns
reads/writes optional
```

6. `atlas_path` 支持 dataflow-lite edge kinds。

### 验收

- 能展示简单 source -> call argument -> callee parameter -> return 的近似路径。
- 所有 dataflow-lite 边标记 provenance/confidence。

---

## 12. 跨里程碑质量要求

每个 milestone 必须：

1. 有 unit tests。
2. 有 MVP language fixtures。
3. 有 error diagnostics。
4. 不引入中心巨型 extractor。
5. 不删除 reference occurrence。
6. 不假装低置信度为高置信度。

---

## 13. 推荐开发顺序

实际落地建议：

```text
1. M0 core/schema + AST dump
2. M1 store + CLI status/init
3. M2 TypeScript/Python extraction first
4. M3 TS/Python resolution first
5. M4 GraphSnapshot callers/callees
6. M6 minimal MCP search/callers/callees/status
7. 回到 M2/M3 补 Java/C/C++/ArkTS/Cangjie
8. M5 context/explore
9. M7 sync
10. M8 dataflow-lite
```

这样能最快得到可被 Agent 使用的闭环。
