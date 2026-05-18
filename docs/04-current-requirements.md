# Atlas 当前需求规格（Rust-native MVP）

> 本文是 Atlas 的当前有效需求规格。早期 “CodeGraph Rust rewrite / 23 语言 feature parity / 旧 schema 兼容” 目标已删除，不再作为实现依据。

---

## 1. 产品定位

Atlas 是一个 **local-first Rust-native 代码知识图谱引擎**。它在本地快速分析代码库，将代码抽取为符号、作用域、引用、调用、类型和依赖关系图谱，并通过 MCP 为 LLM Agent 提供结构化代码智能。

核心服务对象：

```text
LLM Agent
代码审查/理解工具
调用分析工具
依赖/影响面分析工具
未来污点分析工具
```

Atlas 的核心价值：

1. **本地优先**：所有数据存储在项目本地 `.atlas/`，不依赖远端服务。
2. **确定性分析**：基于 tree-sitter AST/query 的可重复抽取，不依赖 AI 猜测生成图谱。
3. **增量更新**：文件变化后只重建受影响部分。
4. **MCP 一等公民**：主要消费方式是 LLM Agent 通过 MCP 查询关系图谱。
5. **可解释关系**：关系带有 `confidence` / `provenance` / `resolved_by`。
6. **面向未来污点分析**：保留 references/callsites/scopes，不只保存最终 edges。

---

## 2. 非目标

MVP 明确不做：

```text
逐行迁移 CodeGraph TypeScript 实现
兼容 .codegraph/codegraph.db schema
23 种语言完整支持
完整编译器级语义分析
完整 C/C++ preprocessing
C++ overload/template 精确解析
Python 动态类型精确推断
Java classpath/Maven/Gradle 完整解析
完整跨过程污点分析
完整 framework resolver 生态
```

MVP 可做 best-effort：

```text
C/C++ include-aware direct call graph
ArkTS via TypeScript grammar fallback
Cangjie grammar-based minimal extraction
低置信度 name-based resolution
```

---

## 3. MVP 语言

MVP 语言固定为：

```text
C
C++
Python
Java
ArkTS
TypeScript
JavaScript
Cangjie
```

后续语言属于 roadmap，不纳入 MVP 验收。

### 3.1 文件扩展映射

```text
.c       -> C
.h       -> C by default, C++ if heuristic matches
.cpp     -> C++
.cc      -> C++
.cxx     -> C++
.hpp     -> C++
.hh      -> C++
.hxx     -> C++
.py      -> Python
.java    -> Java
.ets     -> ArkTS
.ts      -> TypeScript
.js      -> JavaScript
.mjs     -> JavaScript
.cjs     -> JavaScript
.cj      -> Cangjie
.cangjie -> Cangjie
```

### 3.2 ArkTS 策略

MVP：

```text
.ets file -> Language::ArkTS
parser -> tree-sitter-typescript fallback
adapter -> TypeScript-like ArkTS adapter
storage language -> arkts
```

后续可切换 native ArkTS grammar，但不得阻塞 MVP。

### 3.3 Cangjie 策略

MVP 前置 grammar spike：

```text
cargo build grammar
AST dump
最小 function/class/import/call fixture
CangjieAdapter minimal implementation
```

---

## 4. 核心功能需求

### FR-1 文件发现

Atlas 必须支持：

1. 从 project root 扫描 MVP 语言文件。
2. 优先使用 `git ls-files`，遵循 `.gitignore`。
3. 非 git 项目回退 filesystem walk。
4. 支持 include/exclude glob。
5. 支持 `.atlasignore`。
6. 排除常见目录：

```text
.git
.atlas
node_modules
dist
build
out
target
__pycache__
.venv
venv
.gradle
.m2
```

### FR-2 AST 解析与抽取

Atlas 必须使用 native tree-sitter grammar。抽取架构必须是：

```text
tree-sitter queries + LanguageAdapter
```

而不是复刻大型 `GenericExtractor`。

每个文件产出：

```text
FileFacts
  file metadata
  symbols
  scopes
  references
  imports
  exports
  raw_edges
  callsites
  diagnostics
```

### FR-3 符号抽取

MVP 至少抽取：

```text
file
module/package/namespace
class
struct
interface
function
method
field/property
variable/constant where reliable
enum / enum_member where grammar supports
type_alias where grammar supports
import/include
```

符号必须包含：

```text
id
kind
name
qualified_name
symbol_path
file_id
language
range byte offsets
line/column range
signature optional
visibility optional
exported/static/async flags where available
container symbol
scope id
package/namespace where available
```

### FR-4 作用域与容器关系

Atlas 必须抽取或推断：

```text
file contains top-level symbols
namespace/package/module contains symbols
class/struct/interface contains methods/fields
function/method owns body scope
```

输出：

```text
scopes table
contains edges
container_symbol_id
scope_id
```

### FR-5 引用抽取

Atlas 必须保留所有重要 reference occurrence，而不是只存最终边。

引用类型至少包括：

```text
calls
instantiates
references
imports/includes
extends
implements
decorates
returns/type_of where feasible
```

引用字段至少包括：

```text
ref_id
file_id
source_symbol_id optional
scope_id
kind
text
name
receiver optional
arity optional
range
line/col
resolved target optional
confidence/status/resolved_by
```

### FR-6 调用关系

Atlas 必须支持：

```text
function -> function calls
method -> function calls
method -> method calls best-effort
constructor / new -> instantiates
ClassName() -> instantiates for Python when target is class
```

调用边必须保留：

```text
callsite location
reference id
confidence
resolved_by
provenance
```

### FR-7 import/include 关系

语言最低要求：

- TS/JS/ArkTS：relative import、named/default/namespace import、basic re-export。
- Python：`import x`、`import x as y`、`from x import y`、relative import best-effort。
- Java：package declaration、single import、wildcard import、same package lookup。
- C/C++：local include、system include external filter、include-aware symbol lookup。
- Cangjie：import exact / same module best-effort。

### FR-8 resolution

Resolution pipeline：

```text
builtin/external filter
scope-local exact lookup
container/class-local lookup
same-file exact lookup
import/include/package resolver
language-specific module resolver
same namespace/package lookup
framework hook optional
project-wide exact + proximity scoring
fuzzy fallback
```

Resolution 必须输出：

```text
resolved target optional
confidence
strategy/resolved_by
provenance
diagnostics optional
```

### FR-9 持久化

Atlas 使用 SQLite WAL，存储在：

```text
<project_root>/.atlas/atlas.db
```

必须有 migration system。

核心表：

```text
files
symbols
scopes
references
imports
exports optional
edges
callsites
project_metadata
schema_migrations
symbols_fts
```

### FR-10 搜索

必须支持：

```text
exact name
qualified name
FTS5 prefix
LIKE substring
CamelCase boundary
compound terms
bounded fuzzy fallback
kind/language/path filters
```

### FR-11 图查询

必须支持：

```text
neighbors
callers
callees
callgraph
impact
shortest path
usages/references
file dependencies
file dependents
```

查询应优先使用 `GraphSnapshot`，避免每步 hit SQLite。

### FR-12 Context / Explore

Atlas 必须提供面向 LLM Agent 的上下文构建：

```text
natural language task -> symbol-like term extraction
hybrid search
entry point selection
type hierarchy expansion
graph expansion
per-file diversity cap
test/non-production downrank
code block extraction
relationship map
bounded output
```

### FR-13 MCP

MCP 必须支持 JSON-RPC over stdio。

MVP 工具：

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

所有工具支持可选：

```text
projectPath
limit / depth / includeCode / includeLowConfidence where applicable
```

### FR-14 CLI

MVP CLI：

```text
atlas init
atlas index
atlas sync
atlas search
atlas status
atlas files
atlas mcp
atlas doctor
```

### FR-15 增量同步

必须支持：

```text
git status --porcelain fast path
mtime/hash fallback
changed file reindex
removed file cleanup
affected refs re-resolution
GraphSnapshot reload
```

MVP 可以整图 reload snapshot，后续再 partial update。

---

## 5. 非功能需求

### NFR-1 性能

目标：

```text
parallel parse via Rayon
batch SQLite writes
read-mostly MCP queries through GraphSnapshot
bounded memory caches
```

### NFR-2 本地与安全

```text
不上传代码
MCP 只访问 projectPath 内文件
读取代码片段必须 validate path under project root
```

### NFR-3 可解释性

所有非结构边必须携带：

```text
confidence
resolved_by
provenance
reference location
```

### NFR-4 可测试性

每种 MVP 语言必须有 fixtures：

```text
basic definitions
imports/includes
direct calls
class/method calls
inheritance/implements
```

### NFR-5 可扩展性

新增语言应主要新增：

```text
LanguageAdapter
queries/*.scm
fixtures
resolution rules if needed
```

不应修改中心大型 extractor。

---

## 6. MVP 验收标准

MVP 完成时：

1. 8 种语言能识别文件并执行解析路径；Cangjie 至少完成 grammar spike 和 minimal adapter。
2. `atlas index` 能写入 `.atlas/atlas.db`。
3. `atlas search` 能检索符号。
4. `atlas callers/callees` 或 MCP 等价工具可查询基本调用关系。
5. TS/JS/ArkTS/Python/Java import resolution 可用。
6. C/C++ include-aware best-effort resolution 可用。
7. GraphSnapshot 支撑低延迟 graph query。
8. MCP 工具可被 LLM Agent 调用，并输出 bounded context。
9. 关系结果中可见 confidence/provenance。
10. 文档中的 fixtures 通过测试。
