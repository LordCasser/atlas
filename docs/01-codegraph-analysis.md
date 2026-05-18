# CodeGraph 核心机制分析

> 本文档记录对本地 `codegraph` 项目的架构阅读结果：它如何分析 symbol relationships、call graphs、code structure；如何存储这些结构；以及如何通过 MCP 提供检索能力。该文档用于指导 Atlas 的 Rust-native 设计，而不是要求逐行迁移 CodeGraph。

---

## 1. 本地 CodeGraph 版本与关键源码路径

本地 `codegraph/package.json` 显示版本为 `0.7.8`。早期需求文档中提到的 `v0.7.6` 仅作为历史背景。

关键源码路径：

| 关注点 | 路径 |
|---|---|
| 抽取主逻辑 | `codegraph/src/extraction/tree-sitter.ts` |
| 语言配置 | `codegraph/src/extraction/languages/*.ts` |
| 抽取协调器 | `codegraph/src/extraction/index.ts` |
| 类型定义 | `codegraph/src/types.ts` |
| SQLite schema | `codegraph/src/db/schema.sql` |
| DB 查询层 | `codegraph/src/db/queries.ts` |
| 引用解析 orchestrator | `codegraph/src/resolution/index.ts` |
| import resolver | `codegraph/src/resolution/import-resolver.ts` |
| name matcher | `codegraph/src/resolution/name-matcher.ts` |
| framework resolvers | `codegraph/src/resolution/frameworks/*` |
| 图遍历 | `codegraph/src/graph/traversal.ts` |
| 高级图查询 | `codegraph/src/graph/queries.ts` |
| 上下文构建 | `codegraph/src/context/index.ts` |
| MCP server | `codegraph/src/mcp/index.ts` |
| MCP tools | `codegraph/src/mcp/tools.ts` |

---

## 2. CodeGraph 的整体模型

CodeGraph 的核心不是编译器级语义分析，而是：

```text
file discovery
  -> language detection
  -> tree-sitter parse
  -> AST-based symbol extraction
  -> direct structural edges
  -> unresolved references
  -> global reference resolution
  -> resolved graph edges
  -> SQLite search / graph traversal / MCP tools
```

也就是说，CodeGraph 将代码分析拆为两个阶段：

1. **Extraction**：从单个文件 AST 中抽取符号、结构和待解析引用。
2. **Resolution**：在全项目范围内把待解析引用连接到目标符号，生成图边。

这使它可以在不依赖完整编译器或语言服务器的情况下，为 LLM Agent 提供“足够有用”的关系图谱。

---

## 3. Code structure 如何抽取

### 3.1 file node

每个源文件都会生成一个 file node：

```text
id = file:<filePath>
kind = file
name = basename(filePath)
qualifiedName = filePath
filePath = relative path
language = detected language
```

这个 file node 是该文件所有顶层符号的容器。

### 3.2 semantic node stack

`TreeSitterExtractor` 内部维护 `nodeStack`，用于表示当前语义容器栈。

典型结构：

```text
file node
  -> class node
       -> method node
```

每次 `createNode()` 成功创建一个符号节点时，如果 `nodeStack` 有父节点，就立即插入：

```text
parent -> child
kind = contains
```

所以 CodeGraph 的代码结构图本质是 AST 遍历过程中通过语义栈构造出的 `contains` 图。

### 3.3 qualifiedName

CodeGraph 当前的 `qualifiedName` 不包含文件路径，而是来自语义栈中的非 file 节点：

```text
ClassName::methodName
Namespace::ClassName::methodName
```

文件路径单独存放在 `filePath` 字段中。

这样做是为了避免把文件路径污染到 FTS 搜索的 `qualified_name` 字段中。

---

## 4. 节点类型和字段

CodeGraph 定义 22 种 `NodeKind`：

```text
file
module
class
struct
interface
trait
protocol
function
method
property
field
variable
constant
enum
enum_member
type_alias
namespace
parameter
import
export
route
component
```

节点字段：

```text
id
kind
name
qualifiedName
filePath
language
startLine
endLine
startColumn
endColumn
docstring
signature
visibility
isExported
isAsync
isStatic
isAbstract
decorators
typeParameters
updatedAt
```

### 4.1 NodeId 实际算法

CodeGraph 当前并不是用 `file_path + qualified_name` 生成 ID，而是在 `tree-sitter-helpers.ts` 中使用：

```text
sha256(`${filePath}:${kind}:${name}:${line}`).substring(0, 32)
return `${kind}:${hash}`
```

这意味着：

- 同名符号可以通过行号区分。
- 插入/移动代码会导致 ID 抖动。
- ID 带有 kind 前缀。
- 该设计适合简单本地图谱，但不适合需要稳定跨版本追踪的分析。

Atlas 不应无脑照搬该 NodeId 方案，应区分 `FileId` / `SymbolId` / `ReferenceId` / `OccurrenceId`。

---

## 5. Relationship 如何抽取

### 5.1 direct edges

抽取阶段最可靠的直接边是：

```text
contains
```

由 `createNode()` 自动生成。

此外 framework extractors 可能额外直接生成 route/component 等节点和引用。

### 5.2 calls：先 unresolved，后 resolved

函数/方法调用不会在抽取阶段直接连到目标符号。流程是：

```text
call_expression found
  -> determine current caller from nodeStack top
  -> extract calleeName
  -> push unresolved reference:
       fromNodeId = caller id
       referenceName = calleeName
       referenceKind = calls
       line / column = callsite
```

例如：

```ts
function a() {
  b();
}
```

抽取阶段只生成：

```text
from = a
referenceName = b
referenceKind = calls
```

resolution 阶段才生成真实边：

```text
a -> b
kind = calls
metadata = { confidence, resolvedBy }
```

### 5.3 receiver method call

对形如：

```text
obj.method()
ClassName.method()
Module::function()
```

CodeGraph 尽量把 `referenceName` 记录为：

```text
obj.method
ClassName.method
Module::function
```

但对 `this/self/super/cls/parent/static` 等 receiver 会降级为：

```text
method
```

原因是这些 receiver 对全局解析帮助有限，且可能产生误导。

### 5.4 instantiates

对构造表达式：

```text
new Foo(...)
object_creation_expression
instance_creation_expression
```

CodeGraph 生成：

```text
referenceKind = instantiates
referenceName = Foo
```

同时，resolution 阶段有一条边提升规则：

```text
calls -> instantiates
```

如果一个 `calls` 引用解析到 class/struct，就把边提升为 `instantiates`。这对 Python / Ruby 这类 `Foo()` 既可能是函数调用也可能是构造的语言很有用。

### 5.5 extends / implements

类、接口、trait、protocol、struct 等结构会扫描继承/实现语法，例如：

```text
extends_clause
implements_clause
superclass
base_clause
extends_interfaces
trait_bounds
base_list
delegation_specifier
inheritance_specifier
class_heritage
```

抽取阶段生成：

```text
fromNodeId = class/interface/trait id
referenceName = parent/interface/trait name
referenceKind = extends / implements
```

resolution 阶段解析为：

```text
class -> base class       kind = extends
class -> interface/trait  kind = implements
```

还有一条提升规则：

```text
extends -> implements
```

如果目标是 interface/protocol/trait，而源不是 interface/protocol/trait，则改为 `implements`。

### 5.6 imports

Import 处理也分两层：

1. 创建 `import` 节点，方便搜索和展示。
2. 创建 unresolved reference，后续解析成 `imports` 边。

例如 TS：

```ts
import { Foo } from "./foo";
```

抽取阶段创建：

```text
kind = import
name = ./foo
signature = full import statement
```

并创建：

```text
fromNodeId = file node id
referenceName = ./foo
referenceKind = imports
```

Import resolver 后续负责 relative path、alias、default/named/namespace imports 和 re-export chain。

### 5.7 decorators / annotations

CodeGraph 会从 symbol 前面的 decorator/annotation 节点生成：

```text
symbol -> decorator target
kind = decorates
```

通常也是 unresolved reference，经 resolution 后生成真实边。

---

## 6. 语言抽取架构

CodeGraph 表面上是 data-driven：

```text
TreeSitterExtractor + LanguageExtractor config
```

每种语言配置包括：

```text
functionTypes
classTypes
methodTypes
interfaceTypes
structTypes
enumTypes
typeAliasTypes
importTypes
callTypes
variableTypes
fieldTypes
propertyTypes
nameField
bodyField
paramsField
returnField
hooks...
```

但实际 `tree-sitter.ts` 中存在大量语言特例：

- Pascal custom AST handling
- Rust `impl_item`
- Go/Rust receiver type
- C/C++ macro misparse handling
- Python decorators
- Kotlin delegation
- Swift inheritance
- PHP grouped imports
- TypeScript public field arrow functions
- Svelte/Vue/Liquid/DFM custom extraction
- framework extraction hooks

因此 CodeGraph 的 `GenericExtractor` 并不是真正轻量的 data config，它已经成为一个大型中心类。Atlas 不应复刻这个结构。

---

## 7. SQLite 存储结构

CodeGraph 默认存储在：

```text
<project>/.codegraph/codegraph.db
```

### 7.1 nodes

```sql
nodes(
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  qualified_name TEXT NOT NULL,
  file_path TEXT NOT NULL,
  language TEXT NOT NULL,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  start_column INTEGER NOT NULL,
  end_column INTEGER NOT NULL,
  docstring TEXT,
  signature TEXT,
  visibility TEXT,
  is_exported INTEGER DEFAULT 0,
  is_async INTEGER DEFAULT 0,
  is_static INTEGER DEFAULT 0,
  is_abstract INTEGER DEFAULT 0,
  decorators TEXT,
  type_parameters TEXT,
  updated_at INTEGER NOT NULL
)
```

### 7.2 edges

```sql
edges(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source TEXT NOT NULL,
  target TEXT NOT NULL,
  kind TEXT NOT NULL,
  metadata TEXT,
  line INTEGER,
  col INTEGER,
  provenance TEXT DEFAULT NULL
)
```

关键索引：

```sql
idx_edges_kind
idx_edges_source_kind(source, kind)
idx_edges_target_kind(target, kind)
idx_edges_provenance
```

这些索引支撑：

```text
getOutgoingEdges(nodeId, kinds)
getIncomingEdges(nodeId, kinds)
callers/callees/impact/path traversal
```

### 7.3 files

```sql
files(
  path TEXT PRIMARY KEY,
  content_hash TEXT NOT NULL,
  language TEXT NOT NULL,
  size INTEGER NOT NULL,
  modified_at INTEGER NOT NULL,
  indexed_at INTEGER NOT NULL,
  node_count INTEGER DEFAULT 0,
  errors TEXT
)
```

用于增量同步和状态统计。

### 7.4 unresolved_refs

```sql
unresolved_refs(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  from_node_id TEXT NOT NULL,
  reference_name TEXT NOT NULL,
  reference_kind TEXT NOT NULL,
  line INTEGER NOT NULL,
  col INTEGER NOT NULL,
  candidates TEXT,
  file_path TEXT NOT NULL DEFAULT '',
  language TEXT NOT NULL DEFAULT 'unknown'
)
```

Resolution 成功后：

```text
insert edges
remove resolved unresolved_refs
```

Atlas 应改进此点：**不要删除 reference occurrence**，因为调用分析、污点分析、低置信度诊断都需要保留引用位置。

### 7.5 FTS5

CodeGraph 创建：

```sql
nodes_fts(
  id,
  name,
  qualified_name,
  docstring,
  signature,
  content='nodes'
)
```

搜索策略：

```text
1. FTS5 prefix search
2. LIKE substring fallback
3. Levenshtein fuzzy fallback
4. BM25 + kind bonus + path relevance + name match bonus
```

这是 deterministic lexical search，不是 embedding/vector search。

---

## 8. Reference Resolution 机制

`ReferenceResolver.resolveOne()` 大致顺序：

```text
1. built-in / external filter
2. knownNames fast pre-filter
3. framework-specific resolvers
4. import-based resolver
5. name matcher
6. choose highest confidence candidate
```

### 8.1 built-in / external filter

过滤无意义引用：

```text
JS/TS: console, window, document, Promise, Math, JSON, React hooks...
Python: print, len, range, str, list, dict, built-in methods...
Go: stdlib packages and builtins
Pascal: standard units and builtins
```

### 8.2 framework resolvers

CodeGraph 包含大量 framework heuristic，例如：

```text
React
Express
Vue / Nuxt
Svelte / SvelteKit
Go interfaces
Python Flask/decorators
Java Spring
C# ASP.NET
Ruby Rails
Laravel
Rust traits / Cargo workspace
SwiftUI
```

这些 resolver 不是编译器语义，而是基于框架约定和项目目录结构的高价值启发式。

### 8.3 import resolver

Import resolver 支持：

```text
relative import path
extension completion
index file lookup
TS/JS path aliases
hard-coded aliases such as @/, ~/, src/
default import
named import
namespace import
re-export chains
bare specifier / external package filtering
```

JS/TS re-export 支持：

```ts
export { Foo } from "./foo";
export { Foo as Bar } from "./foo";
export * from "./foo";
export * as ns from "./foo";
```

最大 re-export follow depth 为 8。

### 8.4 name matcher

兜底策略：

```text
file path match
qualified name match
method call match
exact name match
fuzzy lowercase match
```

多个候选时按以下信号评分：

```text
same file
path proximity
same language
call target kind preference
instantiation target kind preference
decorator target preference
exported bonus
line proximity
```

---

## 9. Graph traversal 和高级查询

图遍历核心在 `GraphTraverser`。

### 9.1 BFS / DFS

BFS 支持：

```text
maxDepth
edgeKinds
nodeKinds
direction: outgoing / incoming / both
limit
includeStart
```

BFS 会优先排序边：

```text
contains > calls > others
```

### 9.2 callers / callees

Callers：

```text
incoming edges of kinds calls, references, imports
```

Callees：

```text
outgoing edges of kinds calls, references, imports
```

### 9.3 call graph

Call graph 是：

```text
focal node
+ recursive callers
+ recursive callees
```

### 9.4 type hierarchy

类型层次使用：

```text
extends
implements
```

双向遍历：

```text
ancestors: outgoing extends/implements
descendants: incoming extends/implements
```

### 9.5 impact radius

影响面分析：

1. 从目标节点沿 incoming edges 反向找依赖者。
2. 如果目标是 class/interface/struct/trait/protocol/module/enum 等容器，先展开 children。
3. 对 children 的 incoming edges 也纳入影响面。

### 9.6 other queries

高级查询包括：

```text
find usages
shortest path
ancestors/children by contains
file dependencies
file dependents
circular dependencies
dead code
node metrics
```

---

## 10. Context Builder

`codegraph_context` 的核心来自 `ContextBuilder.findRelevantContext()`。

它不是简单搜索，而是多通道混合检索：

```text
1. 从自然语言 query 中提取可能的符号名
2. exact name lookup
3. definition prefix search
4. FTS semantic-ish lexical search
5. multi-term boosting
6. CamelCase boundary LIKE search
7. compound term matching
8. test file down-rank
9. resolve import/export nodes to definitions
10. type hierarchy expansion
11. BFS graph expansion
12. per-file diversity cap
13. non-production file cap
14. restore edges among selected nodes
15. extract code blocks
16. markdown/json formatting
```

这套逻辑是 CodeGraph 对 LLM Agent 真正有价值的部分之一。

---

## 11. MCP server 和工具

MCP server 使用：

```text
JSON-RPC 2.0 over stdio
protocolVersion = 2024-11-05
```

工具：

```text
codegraph_search
codegraph_context
codegraph_callers
codegraph_callees
codegraph_impact
codegraph_node
codegraph_explore
codegraph_status
codegraph_files
```

所有工具支持 `projectPath`，可跨项目查询。

### 11.1 codegraph_search

快速符号搜索，只返回位置和摘要，不返回大段代码。

### 11.2 codegraph_context

主工具。输入自然语言 task，返回 entry points、related symbols、关键代码块。

### 11.3 codegraph_callers / codegraph_callees

先按 symbol name 找所有匹配节点，再聚合 callers/callees。

### 11.4 codegraph_impact

对所有匹配符号合并影响面。

### 11.5 codegraph_node

获取单个符号详情，可选包含完整源代码。

### 11.6 codegraph_explore

深度探索工具。流程：

```text
findRelevantContext(query, depth=3, maxNodes=200)
group nodes by file
score files
build relationship map
read contiguous source sections
merge nearby line ranges
include additional relevant files list
limit output to ~35k chars
```

它的目标是减少 LLM Agent 多次 `Read/Grep` 调用。

### 11.7 codegraph_status / codegraph_files

提供索引状态和文件结构。`codegraph_files` 是替代 filesystem glob 的快速结构查询。

---

## 12. CodeGraph 的优点

1. **产品形态正确**：local-first + deterministic AST + SQLite + MCP。
2. **MCP 工具围绕 LLM Agent 使用场景设计**，尤其 context/explore。
3. **简单可靠的图模型**：nodes / edges / unresolved_refs。
4. **增量同步实用**：git status / mtime fallback。
5. **搜索质量调优充分**：FTS、LIKE、fuzzy、多信号打分。
6. **framework heuristic 提升实际可用性**。

---

## 13. CodeGraph 的局限

1. **不是编译器级语义分析**：重载、泛型、动态类型、宏、模板等都只是 best-effort。
2. **大型 `TreeSitterExtractor` 技术债明显**：语言特例集中在中心类。
3. **resolved references 被删除**：不利于 callsite / taint / low-confidence analysis。
4. **NodeId 对行号敏感**：不适合稳定跨版本跟踪。
5. **图遍历频繁打 SQLite**：对长期 MCP 服务和大项目不是最优。
6. **缺少 scope graph / occurrence model**：污点分析和精细调用分析能力不足。
7. **schema 对未来 dataflow 不够友好**。

---

## 14. 对 Atlas 的启发

Atlas 应保留：

```text
local-first
AST deterministic extraction
SQLite persistence
unresolved -> resolution -> edges pipeline
hybrid search
MCP context/explore tools
incremental sync
confidence/provenance idea
```

Atlas 不应照搬：

```text
巨大 GenericExtractor
CodeGraph NodeId 算法
删除 resolved references
完全相同 SQLite schema
每次图查询都打 SQLite
过早承诺全语言 feature parity
```

最终结论：

> CodeGraph 证明了“轻量 AST 图谱 + 启发式 resolution + MCP 封装”对 LLM Agent 很有价值；Atlas 应吸收其产品经验，但用 Rust-native 的 extraction/query/storage 架构重做底层。
