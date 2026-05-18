# Atlas Rust-native MVP 架构分析与最终结论

> 本文档是对早期迁移架构文档的架构师评估与修正。结论：Atlas 不应作为 CodeGraph 的逐行 Rust rewrite，而应作为 CodeGraph-inspired 的 Rust-native 本地代码知识图谱引擎。

---

## 1. 最终定位

Atlas 的核心目标：

> **本地快速分析代码库，将代码抽取为关系图谱，并通过 MCP 为 LLM Agent 提供符号搜索、调用查询、依赖分析、影响面分析、代码上下文构建，以及未来污点分析所需的基础关系。**

这意味着：

- CodeGraph 是启发来源，不是兼容目标。
- Rust 实现应充分利用类型系统、并发、安全内存模型和 native tree-sitter。
- 数据模型要为未来 callsite/dataflow/taint analysis 预留空间。
- MCP 是一等公民，不是 CLI 的附属功能。

---

## 2. 对早期 docs 的评估

早期文档 `01-04` 做对了：

1. 识别了 CodeGraph 的关键价值：local-first、AST extraction、SQLite、MCP。
2. 选择 native tree-sitter 而不是 WASM。
3. 保留 extraction -> resolution -> query 的阶段化思路。
4. 使用 Rust enum/newtype 替代 TS string union。
5. 使用 SQLite WAL 作为本地持久化方案。
6. 规划了搜索、图遍历、context、sync、MCP 等模块。

但存在关键问题：

1. **过度强调 feature parity**：Atlas 不需要完全兼容 `.codegraph` 行为和 schema。
2. **过度倾向复刻 `GenericExtractor + LangConfig`**：会继承 CodeGraph 中心类技术债。
3. **MVP 范围过大**：23 种语言应为 roadmap，不是 MVP。
4. **schema 不够支持 references/scopes/callsites/dataflow**。
5. **`Mutex<Connection>` 不适合长期读多写少 MCP 服务**。
6. **图遍历直接查 SQLite 会限制性能**。
7. **NodeId 设计需要更稳定、更分层**。

---

## 3. 核心架构原则

### 3.1 Rust-native, not TypeScript-shaped

不要把 TypeScript 里的类结构照搬到 Rust。Rust 版应使用：

```text
trait
newtype IDs
enum state machine
immutable IR
batch writer
read snapshot
Arc / ArcSwap
Rayon for CPU-bound parse
Tokio for MCP I/O
```

### 3.2 Query-driven extraction

不要实现一个 3000 行 `GenericExtractor`。使用：

```text
tree-sitter queries + LanguageAdapter
```

语言差异放在：

```text
queries/<language>/*.scm
src/languages/<language>.rs
```

统一输出 `FileFacts`。

### 3.3 SQLite as source of truth, GraphSnapshot for query

SQLite 负责：

```text
持久化
事务
增量更新
FTS5
跨进程可见性
调试 inspect
```

GraphSnapshot 负责：

```text
MCP 低延迟图查询
BFS/DFS/path/impact/callgraph
减少 SQLite roundtrip
```

### 3.4 Preserve references, not only edges

Atlas 必须保留 reference occurrence。边只是解析后的图关系，reference 是源码事实。

这对以下能力至关重要：

```text
callsite 查询
低置信度诊断
未解析引用展示
污点路径定位
argument/parameter/return 映射
```

### 3.5 Confidence and provenance everywhere

所有 resolution 结果都必须携带：

```text
confidence
resolved_by
provenance
```

LLM Agent 查询时应知道哪些边是精确的，哪些是启发式的。

---

## 4. 推荐模块架构

可以先单 crate 实现，但逻辑上应按以下边界拆分。长期可演进为 workspace。

```text
atlas-core
  ids, kinds, IR, source ranges, config, errors

atlas-languages
  LanguageAdapter trait
  C / C++ / Python / Java / ArkTS / TS / JS / Cangjie adapters
  tree-sitter queries

atlas-extract
  parser registry
  query engine
  scope builder
  file facts builder

atlas-resolve
  scope resolver
  import/include resolver
  package/module resolver
  name matcher
  framework hooks
  confidence model

atlas-store
  SQLite schema
  migrations
  write pipeline
  read API
  FTS

atlas-graph
  GraphSnapshot
  traversal
  call graph
  impact
  path search

atlas-context
  hybrid search
  graph expansion
  code block extraction
  formatter

atlas-sync
  git status / mtime detector
  incremental reindex
  watcher

atlas-mcp
  JSON-RPC stdio
  MCP tools
  output budgeting
  project cache

atlas-cli
  index / sync / search / mcp / doctor
```

单二进制输出仍然是：

```text
atlas
```

---

## 5. 数据流

### 5.1 Index full project

```text
scan files
  -> detect MVP language
  -> parse in parallel
  -> run tree-sitter queries
  -> normalize captures through LanguageAdapter
  -> build scopes and contains edges
  -> emit FileFacts
  -> batch write files/symbols/scopes/references/raw_edges
  -> resolve references
  -> write resolved edges and update references
  -> rebuild / refresh GraphSnapshot
```

### 5.2 Incremental sync

```text
detect changed/deleted files
  -> delete facts for removed/changed files
  -> re-parse changed files
  -> write new FileFacts
  -> re-resolve refs in affected files and dependents
  -> refresh affected graph snapshot sections or reload snapshot
```

MVP 可以全量 reload GraphSnapshot；后续再做 partial update。

### 5.3 MCP query

```text
MCP tool request
  -> resolve project path
  -> ensure DB and GraphSnapshot loaded
  -> search / graph query / context build
  -> read source snippets if needed
  -> return bounded JSON/Markdown
```

---

## 6. Core IR

### 6.1 FileFacts

```rust
pub struct FileFacts {
    pub file: FileInfo,
    pub symbols: Vec<SymbolDef>,
    pub scopes: Vec<ScopeDef>,
    pub references: Vec<ReferenceUse>,
    pub imports: Vec<ImportDef>,
    pub exports: Vec<ExportDef>,
    pub raw_edges: Vec<RawEdge>,
    pub callsites: Vec<Callsite>,
    pub diagnostics: Vec<ExtractDiagnostic>,
}
```

### 6.2 IDs

不要只有 `NodeId`。建议分层：

```rust
FileId
SymbolId
ScopeId
ReferenceId
EdgeId
CallsiteId
OccurrenceId
```

建议：

```text
FileId = blake3(project_relative_path)
SymbolId = blake3(file_id + language + symbol_path + kind + stable discriminator)
ReferenceId = blake3(file_id + source_symbol + byte_range + reference_text)
EdgeId = blake3(source + target + kind + ref_id/provenance)
```

注意：

- 不要只用 line 作为稳定 ID 的核心。
- 存储 byte range 和 line/col，方便精确代码截取。
- 对 overload/template 等，加入 signature discriminator。

### 6.3 SymbolDef

```rust
pub struct SymbolDef {
    pub id: SymbolId,
    pub kind: SymbolKind,
    pub name: String,
    pub qualified_name: String,
    pub symbol_path: String,
    pub file_id: FileId,
    pub language: Language,
    pub range: TextRange,
    pub name_range: TextRange,
    pub signature: Option<String>,
    pub visibility: Option<Visibility>,
    pub exported: bool,
    pub static_: bool,
    pub async_: bool,
    pub container: Option<SymbolId>,
    pub scope_id: ScopeId,
    pub package_name: Option<String>,
    pub namespace_path: Option<String>,
}
```

### 6.4 ReferenceUse

```rust
pub struct ReferenceUse {
    pub id: ReferenceId,
    pub file_id: FileId,
    pub source_symbol: Option<SymbolId>,
    pub scope_id: ScopeId,
    pub kind: ReferenceKind,
    pub text: String,
    pub name: String,
    pub receiver: Option<String>,
    pub arity: Option<u16>,
    pub range: TextRange,
    pub resolved: Option<ResolvedTarget>,
}
```

### 6.5 ResolvedTarget

```rust
pub struct ResolvedTarget {
    pub symbol_id: SymbolId,
    pub confidence: f32,
    pub strategy: ResolutionStrategy,
    pub provenance: Provenance,
}
```

### 6.6 ImportDef

C/C++ include、TS/Python/Java import、Cangjie import 应统一为 dependency declaration。

```rust
pub enum ImportKind {
    Include,
    Import,
    FromImport,
    Package,
    Use,
}

pub struct ImportDef {
    pub kind: ImportKind,
    pub module: String,
    pub imported_name: Option<String>,
    pub local_name: Option<String>,
    pub is_wildcard: bool,
    pub is_relative: bool,
    pub range: TextRange,
}
```

### 6.7 Callsite

为调用分析和未来污点分析预留：

```rust
pub struct Callsite {
    pub id: CallsiteId,
    pub reference_id: ReferenceId,
    pub caller: Option<SymbolId>,
    pub callee: Option<SymbolId>,
    pub receiver: Option<String>,
    pub args: Vec<ArgumentFact>,
    pub range: TextRange,
}
```

MVP 可以只填 `id/reference_id/caller/callee/receiver/range`，args 后续补。

---

## 7. SQLite schema 建议

`.atlas/atlas.db` 不需要兼容 `.codegraph/codegraph.db`。

### 7.1 files

```sql
CREATE TABLE files (
    file_id BLOB PRIMARY KEY,
    path TEXT UNIQUE NOT NULL,
    language TEXT NOT NULL,
    module_name TEXT,
    package_name TEXT,
    is_header INTEGER NOT NULL DEFAULT 0,
    content_hash TEXT NOT NULL,
    size INTEGER NOT NULL,
    modified_at INTEGER NOT NULL,
    indexed_at INTEGER NOT NULL,
    parse_status TEXT NOT NULL,
    parse_errors TEXT
);
```

### 7.2 symbols

```sql
CREATE TABLE symbols (
    symbol_id BLOB PRIMARY KEY,
    file_id BLOB NOT NULL,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    qualified_name TEXT NOT NULL,
    symbol_path TEXT NOT NULL,
    package_name TEXT,
    namespace_path TEXT,
    container_symbol_id BLOB,
    scope_id BLOB,
    signature TEXT,
    visibility TEXT,
    exported INTEGER NOT NULL DEFAULT 0,
    is_async INTEGER NOT NULL DEFAULT 0,
    is_static INTEGER NOT NULL DEFAULT 0,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    start_col INTEGER NOT NULL,
    end_col INTEGER NOT NULL,
    FOREIGN KEY(file_id) REFERENCES files(file_id) ON DELETE CASCADE
);
```

### 7.3 scopes

```sql
CREATE TABLE scopes (
    scope_id BLOB PRIMARY KEY,
    file_id BLOB NOT NULL,
    parent_scope_id BLOB,
    owner_symbol_id BLOB,
    kind TEXT NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    FOREIGN KEY(file_id) REFERENCES files(file_id) ON DELETE CASCADE
);
```

### 7.4 references

```sql
CREATE TABLE references (
    ref_id BLOB PRIMARY KEY,
    file_id BLOB NOT NULL,
    source_symbol_id BLOB,
    scope_id BLOB,
    kind TEXT NOT NULL,
    text TEXT NOT NULL,
    name TEXT NOT NULL,
    receiver TEXT,
    arity INTEGER,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    line INTEGER NOT NULL,
    col INTEGER NOT NULL,
    resolved_symbol_id BLOB,
    confidence REAL,
    resolved_by TEXT,
    status TEXT NOT NULL,
    FOREIGN KEY(file_id) REFERENCES files(file_id) ON DELETE CASCADE
);
```

### 7.5 imports

```sql
CREATE TABLE imports (
    import_id BLOB PRIMARY KEY,
    file_id BLOB NOT NULL,
    kind TEXT NOT NULL,
    module TEXT NOT NULL,
    imported_name TEXT,
    local_name TEXT,
    is_wildcard INTEGER NOT NULL DEFAULT 0,
    is_relative INTEGER NOT NULL DEFAULT 0,
    resolved_file_id BLOB,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    FOREIGN KEY(file_id) REFERENCES files(file_id) ON DELETE CASCADE
);
```

### 7.6 edges

```sql
CREATE TABLE edges (
    edge_id BLOB PRIMARY KEY,
    source_id BLOB NOT NULL,
    target_id BLOB NOT NULL,
    kind TEXT NOT NULL,
    ref_id BLOB,
    confidence REAL NOT NULL,
    provenance TEXT NOT NULL,
    metadata TEXT,
    line INTEGER,
    col INTEGER
);
```

### 7.7 callsites

```sql
CREATE TABLE callsites (
    callsite_id BLOB PRIMARY KEY,
    ref_id BLOB NOT NULL,
    caller_symbol_id BLOB,
    callee_symbol_id BLOB,
    receiver TEXT,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    line INTEGER NOT NULL,
    col INTEGER NOT NULL
);
```

### 7.8 FTS

```sql
CREATE VIRTUAL TABLE symbols_fts USING fts5(
    symbol_id,
    name,
    qualified_name,
    symbol_path,
    signature,
    content='symbols'
);
```

可增加 trigram/LIKE 辅助表，或先用普通 indexes + FTS5。

### 7.9 关键索引

```sql
CREATE INDEX idx_symbols_name ON symbols(name);
CREATE INDEX idx_symbols_qname ON symbols(qualified_name);
CREATE INDEX idx_symbols_file ON symbols(file_id);
CREATE INDEX idx_symbols_kind ON symbols(kind);
CREATE INDEX idx_symbols_package ON symbols(package_name);
CREATE INDEX idx_symbols_namespace ON symbols(namespace_path);

CREATE INDEX idx_refs_name ON references(name);
CREATE INDEX idx_refs_file ON references(file_id);
CREATE INDEX idx_refs_source ON references(source_symbol_id);
CREATE INDEX idx_refs_resolved ON references(resolved_symbol_id);
CREATE INDEX idx_refs_status ON references(status);

CREATE INDEX idx_edges_source_kind ON edges(source_id, kind);
CREATE INDEX idx_edges_target_kind ON edges(target_id, kind);
CREATE INDEX idx_edges_kind ON edges(kind);

CREATE INDEX idx_imports_file ON imports(file_id);
CREATE INDEX idx_imports_module ON imports(module);
```

---

## 8. EdgeKind 设计

CodeGraph 的 12 种边适合符号图：

```text
contains
calls
imports
exports
extends
implements
references
type_of
returns
instantiates
overrides
decorates
```

Atlas MVP 应至少支持这些，并为未来 dataflow 扩展预留：

```text
defines
argument
parameter
assigns
reads
writes
field_read
field_write
```

建议内部 EdgeKind：

```rust
pub enum EdgeKind {
    Contains,
    Calls,
    Imports,
    Includes,
    Exports,
    Extends,
    Implements,
    References,
    TypeOf,
    Returns,
    Instantiates,
    Overrides,
    Decorates,
    Defines,
    Argument,
    Parameter,
    Assigns,
    Reads,
    Writes,
    FieldRead,
    FieldWrite,
}
```

MCP 可以默认隐藏 dataflow-lite 边，除非工具显式请求。

---

## 9. Extraction 架构

### 9.1 LanguageAdapter trait

```rust
pub trait LanguageAdapter: Send + Sync {
    fn language(&self) -> Language;
    fn extensions(&self) -> &'static [&'static str];
    fn tree_sitter_language(&self) -> tree_sitter::Language;

    fn definition_query(&self) -> &'static str;
    fn reference_query(&self) -> &'static str;
    fn import_query(&self) -> &'static str;
    fn scope_query(&self) -> Option<&'static str> { None }

    fn normalize_definition(
        &self,
        captures: CaptureSet,
        source: &SourceFile,
    ) -> Option<SymbolDef>;

    fn normalize_reference(
        &self,
        captures: CaptureSet,
        source: &SourceFile,
    ) -> Option<ReferenceUse>;

    fn normalize_import(
        &self,
        captures: CaptureSet,
        source: &SourceFile,
    ) -> Option<ImportDef>;
}
```

### 9.2 Query files

建议路径：

```text
src/languages/queries/typescript/definitions.scm
src/languages/queries/typescript/references.scm
src/languages/queries/typescript/imports.scm
src/languages/queries/python/...
src/languages/queries/java/...
src/languages/queries/c/...
src/languages/queries/cpp/...
src/languages/queries/arkts/...
src/languages/queries/cangjie/...
```

### 9.3 Extractor flow

```text
parse source
  -> execute definition query
  -> normalize symbol captures
  -> execute scope query
  -> build scope tree
  -> execute import query
  -> normalize imports
  -> execute reference query
  -> normalize references and callsites
  -> infer contains edges from ranges / scopes
  -> return FileFacts
```

---

## 10. Resolution 架构

CodeGraph 的 `framework -> import -> name` 三阶段应升级为 scope-aware pipeline。

建议顺序：

```text
1. builtin/external filter
2. scope-local exact lookup
3. container/class-local lookup
4. same-file exact lookup
5. import/include/package resolver
6. language-specific module resolver
7. same namespace/package exact lookup
8. framework resolver
9. project-wide exact + proximity scoring
10. fuzzy fallback
```

### 10.1 Confidence tiers

```text
1.00 compiler/scip/lsp exact, if future supported
0.95 same-scope exact / exact qualified name
0.90 import exact / package exact
0.80 framework convention / namespace proximity
0.70 same-file or same-package name match
0.50 fuzzy / ambiguous fallback
<0.50 unresolved or speculative
```

MCP 默认可只展示：

```text
confidence >= 0.7
```

并允许参数：

```text
includeLowConfidence = true
```

### 10.2 Resolution output

```rust
pub struct Resolution {
    pub ref_id: ReferenceId,
    pub target: Option<SymbolId>,
    pub confidence: f32,
    pub strategy: ResolutionStrategy,
    pub diagnostics: Vec<ResolutionNote>,
}
```

---

## 11. Store / DB concurrency

早期文档的 `Mutex<Connection>` 简单但不理想。

推荐：

```text
Write path:
  single writer connection / write actor / batch transactions

Read path:
  read-only connection pool or short-lived read connections

MCP path:
  GraphSnapshot for graph queries
  SQLite read API for source metadata / FTS / details
```

SQLite WAL 允许多 reader 单 writer。Rust 应利用这一点。

MVP 简化方案：

```text
DatabaseWriter with Mutex<Connection>
DatabaseReader opens read-only connections as needed
GraphSnapshot reload after index/sync
```

后续再优化为 actor/pool。

---

## 12. GraphSnapshot

### 12.1 目的

避免每次 BFS/DFS 都大量查询 SQLite。

### 12.2 结构

```rust
pub struct GraphSnapshot {
    pub nodes: Vec<NodeSummary>,
    pub edges: Vec<EdgeSummary>,
    pub id_to_idx: HashMap<SymbolId, NodeIx>,
    pub name_index: HashMap<String, Vec<NodeIx>>,
    pub qname_index: HashMap<String, Vec<NodeIx>>,
    pub file_index: HashMap<FileId, Vec<NodeIx>>,
    pub outgoing: Vec<Vec<EdgeIx>>,
    pub incoming: Vec<Vec<EdgeIx>>,
}
```

### 12.3 Query API

```rust
pub trait GraphQuery {
    fn neighbors(&self, id: SymbolId, config: TraversalConfig) -> Subgraph;
    fn callers(&self, id: SymbolId, depth: u8) -> CallGraph;
    fn callees(&self, id: SymbolId, depth: u8) -> CallGraph;
    fn impact(&self, id: SymbolId, depth: u8) -> Subgraph;
    fn shortest_path(&self, from: SymbolId, to: SymbolId, kinds: EdgeKindSet) -> Option<Path>;
    fn usages(&self, id: SymbolId) -> Vec<ReferenceSummary>;
}
```

MVP 可使用自定义 adjacency vectors。暂不需要引入复杂图数据库。

---

## 13. Search / Context

### 13.1 Symbol search

保留 CodeGraph 已验证的混合策略：

```text
exact name
qualified name
FTS5 prefix
LIKE substring
CamelCase boundary
compound term
bounded Levenshtein
kind/path/language scoring
```

### 13.2 Context builder

Pipeline：

```text
parse natural language query
  -> extract symbol-like terms
  -> run hybrid search
  -> choose entry points
  -> resolve import/export nodes to definitions
  -> type hierarchy expansion
  -> graph traversal
  -> per-file diversity cap
  -> test/non-production cap
  -> restore edges between selected nodes
  -> read code blocks by byte/line ranges
  -> output Markdown/JSON under budget
```

### 13.3 Explore

`atlas_explore` 应类似 CodeGraph 的 explore，但输出更结构化：

```text
summary
relationship map
source code sections grouped by file
additional files
confidence warnings
budget note
```

---

## 14. MCP 工具设计

MVP 工具建议：

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

### 14.1 atlas_neighbors

通用邻居查询，可替代很多专用工具。

输入示例：

```json
{
  "symbol": "AuthService.login",
  "direction": "both",
  "edgeKinds": ["calls", "references", "contains"],
  "depth": 2,
  "limit": 100
}
```

### 14.2 atlas_path

用于查调用链/依赖链/未来污点路径。

```json
{
  "from": "parseRequest",
  "to": "executeQuery",
  "edgeKinds": ["calls", "returns", "argument", "assigns"],
  "maxDepth": 8
}
```

### 14.3 atlas_context

主工具，用自然语言 task 构建上下文。

### 14.4 atlas_explore

深度探索工具，适合 LLM Agent 一次性理解代码区域。

### 14.5 Future: atlas_taint_trace

MVP 可以先不承诺完整 taint，但 schema 和 callsite/reference/dataflow-lite 要支持后续实现：

```json
{
  "source": "req.body",
  "sink": "executeQuery",
  "maxDepth": 10,
  "includeCode": true
}
```

---

## 15. CLI 命令

MVP CLI：

```text
atlas init [--project <path>]
atlas index [--project <path>] [--include <glob>] [--exclude <glob>]
atlas sync [--project <path>]
atlas search <query> [--kind <kind>] [--language <lang>] [--limit <n>]
atlas status [--project <path>]
atlas files [--project <path>] [--format tree|flat|grouped]
atlas mcp [--project <path>]
atlas doctor
```

`doctor` 用于检查：

```text
SQLite FTS5 availability
tree-sitter grammar availability
Cangjie grammar build status
project root / .atlas status
schema version
```

---

## 16. MVP 里不做或只 best-effort 的事项

### 不做

```text
完整编译器级类型检查
完整 C/C++ preprocessing
C++ overload/template 精确解析
Python 动态类型精确推断
Java classpath 完整解析
完整跨过程 taint analysis
所有 CodeGraph framework resolvers
全 23 语言支持
.codegraph DB binary compatibility
```

### best-effort

```text
C/C++ direct calls
C/C++ includes
C++ namespace/class method lookup
Python class constructor promotion
ArkTS through TypeScript parser
Cangjie grammar-based extraction
```

---

## 17. 推荐实现里程碑

### M0: IR / schema / grammar spike

- 定义 FileId/SymbolId/ReferenceId/EdgeId。
- 定义 FileFacts IR。
- 设计并创建 SQLite schema。
- 验证 MVP 语言 grammar，尤其 Cangjie。
- 实现 AST dump debug 工具。

### M1: Store + basic CLI

- `.atlas/atlas.db` 初始化。
- migrations。
- insert/query files/symbols/references/edges。
- `atlas status`。

### M2: Query-driven extraction MVP

- TS/JS/Python/Java 基础 definitions/imports/references。
- C/C++ 基础 definitions/includes/calls。
- ArkTS 复用 TS parser。
- Cangjie 最小 definitions/imports/calls。
- 输出 FileFacts 并写 DB。

### M3: Resolution MVP

- same-file/scope resolution。
- TS/JS/ArkTS import resolution。
- Python import resolution。
- Java package/import resolution。
- C/C++ include-aware resolution。
- Cangjie same module/import resolution。
- 生成 calls/imports/includes/extends/implements/instantiates edges。

### M4: GraphSnapshot + graph queries

- load nodes/edges into memory snapshot。
- callers/callees/neighbors/impact/path。
- confidence filtering。

### M5: MCP MVP

- atlas_status/files/search/symbol/neighbors/callers/callees/impact/context/explore。
- 输出预算控制。
- projectPath 支持。

### M6: Incremental sync

- git status / mtime。
- changed file reindex。
- affected refs re-resolve。
- GraphSnapshot reload。

### M7: dataflow-lite foundation

- callsite arguments。
- params/returns。
- assignment facts。
- `atlas_path` 支持 dataflow-lite 边。

---

## 18. 最终结论

Atlas 应该保留 CodeGraph 证明有效的产品体验：

```text
local-first
AST graph
SQLite
MCP
hybrid search
context/explore
incremental sync
```

但底层应改成 Rust-native：

```text
tree-sitter queries + LanguageAdapter
stable multi-ID model
symbols + scopes + references + edges + callsites
SQLite source of truth
GraphSnapshot query acceleration
scope-aware resolution
confidence/provenance everywhere
MVP language focus
```

一句话：

> **Atlas 不应是 CodeGraph 的 Rust 影子，而应是一个 Rust-native 本地代码知识图谱引擎，为 LLM Agent 的调用分析、依赖分析、影响面分析和未来污点分析提供高性能、可解释、可增量更新的 MCP 服务。**
