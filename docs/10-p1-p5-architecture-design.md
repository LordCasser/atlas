# Atlas P1-P5 架构设计文档

> 渐进式改造方案，基于已有 P0 稳定基线
> 决策策略：渐进式改造 (A) + P2 末分离 Resolver/GraphBuilder (A) + P3 拆分 edges 表 (A)

---

## 0. 总体设计原则

1. **渐进式**：每阶段可独立测试，不中断现有功能
2. **SemanticBinder 是单一权威**：source_symbol、scope_id、binding 由它统一填充
3. **Resolver 不创建 edges**（P2末实现）：Resolver 只输出 resolved facts，GraphBuilder 创建 edges
4. **DataFlow 不用 SymbolId 伪装**（P3实现）：新增 DataNode/DataFlowEdge，DataNodeId → DataNodeId
5. **edges 表只保留 symbol-level**（P3实现）：symbol_edges 和 dataflow_edges 分表
6. **Graph 分层加载**（P3+）：SymbolGraph 全量，DataFlowGraph/CFG 按函数加载

---

## P1：追平 CodeGraph 产品化索引能力

### 目标
大项目可用，失败可控，Agent 体验友好。

### 模块结构（改动在现有模块内）

```
src/
  cli/commands/
    index.rs              ← 增加 --max-file-size, --timeout 参数
  sync/
    discovery.rs          ← 增加 .atlasignore, max file size, generated file skip
    mod.rs                ← 增加 file lock (SQLite-based 或 flock)
  extraction/
    extract.rs            ← 增加 per-file timeout, error 分类聚合
    worker.rs             ← 新增：ParseWorkerPool (Rayon + panic::catch_unwind)
  search/
    query_parser.rs       ← 新增：field query 语法解析
    mod.rs                ← 集成 query parser 到 SearchEngine
  types/
    structs.rs            ← IndexReport, FailureCategory 结构体
  db/
    store.rs              ← 增加 index_report 写入
tests/
  fixtures/               ← 新增：per-language golden test fixtures
    typescript/
      simple.ts
      simple.expected.json
    python/
      simple.py
      simple.expected.json
    java/
      ...
    ...
```

### 新增类型

```rust
// src/types/structs.rs 新增

/// 索引报告 — 写入 .atlas/index_report.json
pub struct IndexReport {
    pub files_discovered: usize,
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub failures_by_category: HashMap<String, usize>,
    pub references_total: usize,
    pub references_resolved: usize,
    pub resolution_rate: f64,
    pub duration_ms: u64,
}

pub enum FailureCategory {
    ParseTimeout,
    QueryError,
    IoError,
    MaxFileSizeExceeded,
    GrammarPanic,
}
```

### 核心组件

#### 1. ParseWorkerPool (src/extraction/worker.rs)

```
职责：管理 Rayon 线程池，每个文件独立 parse + extract
能力：
  - per-file timeout (默认 30s, 可配置)
  - panic::catch_unwind 隔离 grammar 崩溃
  - 失败文件标记 skipped，不中断整个 index
  - 结构化错误收集 (not best-effort silent)
```

接口设计：
```rust
pub struct ParseWorkerPool {
    max_file_size: Option<u64>,
    timeout: Duration,
    report: IndexReportCollector,
}

impl ParseWorkerPool {
    pub fn new(config: WorkerConfig) -> Self;
    pub fn extract_file(&self, adapter: &dyn LanguageAdapter, file_id: FileId, ...) -> Result<FileFacts, ExtractionError>;
    pub fn report(&self) -> &IndexReportCollector;
}

pub struct WorkerConfig {
    pub max_file_size_bytes: Option<u64>,
    pub parse_timeout_secs: u64,
    pub max_workers: usize,
}
```

#### 2. FileLock (src/sync/mod.rs)

```
SQLite-based：使用 project_metadata 表保存 lock
或 flock-based：操作系统文件锁

接口：
  FileLock::acquire(db_path) -> Result<FileLockGuard>
  FileLockGuard: Drop 时释放锁
```

#### 3. SearchQueryParser (src/search/query_parser.rs)

```
语法：kind:<kind> lang:<lang> path:<path> name:<name> <freetext>
示例：kind:function lang:typescript path:src name:auth authenticate

解析为 SearchOptions { kind_filter, language, path_filter, name_filter, freetext }
```

#### 4. Golden Test Framework (tests/fixtures/)

```
每个语言的 fixture 目录结构：
  tests/fixtures/<lang>/
    simple.<ext>              ← 输入源码
    simple.expected.json      ← 期望的 symbols/references/imports/scopes/callsites

Expected JSON 格式：
{
  "symbols": [{ "name": "foo", "kind": "function", "range": "1:1-3:1" }],
  "references": [{ "text": "bar", "kind": "call", "range": "2:5-2:8" }],
  "imports": [...],
  "scopes": [...],
  "callsites": [...]
}

测试宏：
  golden_test!("typescript/simple.ts")
  - 解析源码，运行 extraction pipeline
  - 比较输出与 expected.json
  - 不匹配时报 diff

覆盖率要求：每个语言至少覆盖:
  - function/class/variable definitions
  - call/field access/type reference
  - import/export
  - scope nesting
  - callsite derivation
```

### 约束
- 不改变 extraction pipeline 的语义逻辑
- 不新增 DB 表
- 不改变 Resolver 行为
- golden test 只验证 extraction 阶段输出（不涉及 resolution）

### 验收
```bash
cargo test --all-features  # 必须全绿
# 大项目 index 不因单文件失败中断
# CLI 输出清晰：discovered/skipped/failed/resolved
```

---

## P2：模块解析与 Call Graph 精度

### 目标
跨文件、跨模块调用关系稳定。Resolver 与 GraphBuilder 分离。

### 模块结构

```
src/
  resolution/
    mod.rs                ← 重构：Resolver 停止创建 edges
    import_resolver.rs    ← 扩展：TS path alias, Python package, Java, C/C++
    export_resolver.rs    ← 新增：re-export/barrel
    member_resolver.rs    ← 新增：obj.method, this.field
    call_resolver.rs      ← 新增：callsite callee 候选
    path_alias.rs         ← 新增：tsconfig.json paths/baseUrl
    include_graph.rs      ← 新增：C/C++ include graph
  graph/
    graph_builder.rs      ← 新增：根据 resolved facts 构建 edges
    mod.rs                ← GraphEngine 适配 GraphBuilder
  db/
    store.rs              ← 增加 resolved fact invalidation API
    schema.rs             ← schema version bump if edges FK changes
```

### 核心重构

#### 1. Resolver/GraphBuilder 分离

**变更前（当前）：**
```
ReferenceResolver::resolve_all()
  ├── resolve_one() → ResolvedTarget
  ├── create_edges() → Vec<RawEdge>   ← 混在一起
  └── flush_batch(resolutions, edges)
```

**变更后：**
```
ReferenceResolver::resolve_all()  → Vec<(ReferenceId, ResolvedTarget)>
  └── 只更新 references."resolved_*" 字段

GraphBuilder::build()
  ├── create_symbol_edges(resolved_refs) → Vec<RawEdge>
  ├── create_call_edges(resolved_refs)   → Vec<RawEdge>
  └── write_edges(edges)
```

**GraphBuilder 职责：**
```rust
pub struct GraphBuilder {
    store: Arc<Store>,
}

impl GraphBuilder {
    /// 从 resolved references 构建 symbol-level edges
    pub fn build_symbol_edges(&self, resolved: &[(ReferenceId, ResolvedTarget)]) -> Vec<RawEdge>;
    /// 从 resolved call references 构建 call edges
    pub fn build_call_edges(&self, resolved: &[(ReferenceId, ResolvedTarget)]) -> Vec<RawEdge>;
    /// 全量构建
    pub fn build_all(&self, resolved: &[(ReferenceId, ResolvedTarget)]) -> Vec<RawEdge>;
}
```

Edges 种类（symbol-level 保留在 `edges` 表）：
- Contains, Calls, Extends, Implements, References, Imports, Exports, Overrides, Decorates

#### 2. Resolved Fact Invalidation

当文件被修改或删除时：
```
1. 找到该文件的所有 references (WHERE file_id = ?)
2. 清空 resolved_symbol_id / resolved_confidence / resolved_strategy / resolved_provenance
3. 找到以该文件 references 为 ref_id 的所有 edges → 删除
4. 如果文件被删除：CASCADE 删除所有 facts
5. 重跑受影响 references 的 resolution
```

Store API 新增：
```rust
impl Store {
    /// 清空指定文件所有 references 的 resolution 结果
    pub fn invalidate_references_for_file(&self, file_id: FileId) -> Result<usize>;
    /// 删除以指定文件 references 为来源的 edges
    pub fn delete_edges_for_file_references(&self, file_id: FileId) -> Result<usize>;
}
```

#### 3. Import Resolution 增强

**TS/JS path alias:**
```rust
pub struct PathAliasResolver {
    base_url: Option<String>,
    paths: HashMap<String, Vec<String>>,  // tsconfig.json "paths"
}

impl PathAliasResolver {
    pub fn from_tsconfig(path: &Path) -> Result<Self>;
    pub fn resolve(&self, import_path: &str) -> Option<PathBuf>;
}
```

**Re-export/barrel:**
```rust
// export_resolver.rs
pub struct ExportResolver {
    store: Arc<Store>,
}

impl ExportResolver {
    /// 解析 import chain 到最终符号
    /// export { x as y } from './m'  →   ./m 的 x
    /// export * from './m'           →   ./m 的所有 exports
    pub fn resolve_reexport(&self, import: &ImportDef) -> Vec<SymbolId>;
}
```

**C/C++ include graph:**
```rust
pub struct IncludeGraph {
    // 区分 system include (#include <...>) 和 local include (#include "...")
    // header/source 配对
    // declaration/definition 合并
}
```

**每个语言增加有针对性的 golden tests：**
```
tests/fixtures/typescript/
  reexport.expected.json
  path_alias.expected.json
tests/fixtures/python/
  relative_import.expected.json
  package.expected.json
tests/fixtures/java/
  static_import.expected.json
tests/fixtures/c/
  include_chain.expected.json
```

### 约束
- `edges` 表仍保留（symbol-level），P3 再拆分
- 不新增 BindingDef/DataNode
- Resolver 仍返回 `Vec<(ReferenceId, ResolvedTarget)>`，GraphBuilder 独立构建 edges

### 验收
- 跨文件调用图稳定（test_cross_file_callers_callees_graph 通过）
- rename/delete 后 sync 不保留悬空 resolved target
- GraphSnapshot 中 symbol-level edges 不指向不存在 symbol
- TS re-export、path alias 能正确 resolve

---

## P3：BindingGraph 与 DataFlowGraph

### 目标
为污点分析打地基。建立局部变量/参数/表达式的精确数据流。

### 模块结构

```
src/
  types/
    bindings.rs           ← 新增：BindingDef, BindingUse, BindingId, BindingUseId, BindingKind
    dataflow.rs           ← 新增：DataNode, DataFlowEdge, DataNodeId, DataFlowEdgeId, DataNodeKind, DataFlowKind
    structs.rs            ← 扩展：CallsiteArg, ReferenceUse 增加 binding_id
    mod.rs                ← 更新导出
  extraction/
    semantic_binder.rs    ← 扩展：增加 bind_bindings() 方法
    lexical_binder.rs     ← 新增：参数/局部变量/import alias/解构绑定提取
    dataflow_builder.rs   ← 新增：函数内 dataflow 提取
    extract.rs            ← 更新：调用 LexicalBinder, DataFlowBuilder
  db/
    schema.rs             ← version bump，新增 bindings, binding_uses, data_nodes, dataflow_edges, callsite_args
    store.rs              ← 新增写入/查询 API
  graph/
    binding_graph.rs      ← 新增：BindingGraph 按函数加载
    dataflow_graph.rs     ← 新增：DataFlowGraph 按函数/切片加载
    mod.rs                ← 移除旧的 dataflow edges 加载逻辑
```

### 新增类型

```rust
// ================================================================
// src/types/bindings.rs
// ================================================================

pub struct BindingId([u8; 32]);
impl BindingId {
    /// blake3(file_id + scope_id + kind + name + start_byte)
    pub fn generate(file_id: &FileId, scope_id: &ScopeId, kind: &BindingKindType, name: &str, start_byte: u32) -> Self;
}

pub struct BindingUseId([u8; 32]);
impl BindingUseId {
    /// blake3(file_id + binding_id? + reference_id? + name + start_byte)
    pub fn generate(file_id: &FileId, binding_id: Option<&BindingId>, reference_id: Option<&ReferenceId>, name: &str, start_byte: u32) -> Self;
}

pub struct BindingDef {
    pub id: BindingId,
    pub file_id: FileId,
    pub function_id: Option<SymbolId>,  // 所属函数
    pub scope_id: ScopeId,
    pub kind: BindingKind,
    pub name: String,
    pub symbol_id: Option<SymbolId>,    // 如果对应 project-level symbol
    pub range: TextRange,
}

pub enum BindingKind {
    Parameter,
    Local,
    Field,
    ImportAlias,
    CatchVariable,
    LambdaParameter,
    Global,
}

pub struct BindingUse {
    pub id: BindingUseId,
    pub file_id: FileId,
    pub scope_id: ScopeId,
    pub binding_id: Option<BindingId>,   // 指向 BindingDef
    pub reference_id: Option<ReferenceId>, // 关联 reference
    pub name: String,
    pub range: TextRange,
}
```

```rust
// ================================================================
// src/types/dataflow.rs
// ================================================================

pub struct DataNodeId([u8; 32]);
impl DataNodeId {
    /// blake3(file_id + function_id? + kind + name? + access_path? + start_byte)
    pub fn generate(file_id, function_id, kind, name, access_path, start_byte) -> Self;
}

pub struct DataFlowEdgeId([u8; 32]);
impl DataFlowEdgeId {
    /// blake3(source + target + kind)
    pub fn generate(source: &DataNodeId, target: &DataNodeId, kind: &DataFlowKindType) -> Self;
}

pub struct DataNode {
    pub id: DataNodeId,
    pub file_id: FileId,
    pub function_id: Option<SymbolId>,
    pub kind: DataNodeKind,
    pub binding_id: Option<BindingId>,
    pub callsite_id: Option<CallsiteId>,
    pub name: Option<String>,
    pub access_path: Option<String>,
    pub range: TextRange,
}

pub enum DataNodeKind {
    Parameter,
    Local,
    Field,
    Return,
    Literal,
    Expr,
    CallArg,
    CallReturn,
    Receiver,
    Global,
    Unknown,
}

pub struct DataFlowEdge {
    pub id: DataFlowEdgeId,
    pub source: DataNodeId,    // ← 不是 SymbolId!
    pub target: DataNodeId,    // ← 不是 SymbolId!
    pub kind: DataFlowKind,
    pub location: TextRange,
    pub confidence: Confidence,
}

pub enum DataFlowKind {
    Assign,
    Read,
    Write,
    FieldLoad,
    FieldStore,
    ArgToParam,
    ReturnToCall,
    ReceiverToThis,
    Phi,
    Sanitized,
}
```

```rust
// ================================================================
// src/types/structs.rs 扩展
// ================================================================

// CallsiteArg — 替代当前 Callsite.args (基本为空)
pub struct CallsiteArg {
    pub callsite_id: CallsiteId,
    pub index: u32,
    pub name: Option<String>,
    pub expr_text: String,
    pub data_node: DataNodeId,
    pub range: TextRange,
}

// ReferenceUse 扩展 — 增加 binding_id
// (在原 struct 中增加字段)
pub struct ReferenceUse {
    // ... 现有字段 ...
    pub binding_id: Option<BindingId>,   // ← 新增：指向词法绑定
}
```

### 新增组件

#### 1. LexicalBinder (src/extraction/lexical_binder.rs)

```
输入：tree-sitter AST + scope tree + SemanticBinder
输出：Vec<BindingDef> + Vec<BindingUse>

职责：
  - 从 AST 提取参数绑定 (function params, lambda params)
  - 从 AST 提取局部变量 (let/const/var, 赋值模式)
  - 从 AST 提取 import alias 绑定
  - 从 AST 提取 catch variable 绑定
  - 从 AST 提取 destructuring 绑定
  - 建立 identifier use → binding def 的关系
  - 处理 shadowing 规则

初期用 tree-sitter query 提取，后续可迁移到 AST walker
```

#### 2. DataFlowBuilder (src/extraction/dataflow_builder.rs)

```
输入：tree-sitter AST + BindingGraph + SemanticBinder
输出：Vec<DataNode> + Vec<DataFlowEdge>

职责：
  - 为每个 binding 创建 DataNode (Parameter, Local, Field)
  - 为每个 return statement 创建 DataNode (Return)
  - 为每个 call argument 创建 DataNode (CallArg)
  - 为每个 literal 创建 DataNode (Literal)
  - 建立函数内 dataflow edges:
    - assignment lhs → rhs:       name = expr  →  DataNode(name) -> DataNode(expr)
    - member access chain:        req.body.name  →  req -> req.body -> req.body.name
    - return flow:                return expr  →  DataNode(expr) -> DataNode(return)
    - call arg flow:              sink(name)  →  DataNode(name) -> DataNode(sink.arg0)
    - field read/write:           obj.field  →  DataNode(obj) -> DataNode(obj.field)

不做跨函数 dataflow（交给 P4 interprocedural）
```

#### 3. SemanticBinder 扩展

```rust
impl SemanticBinder {
    // 现有方法不变
    pub fn bind_source(&self, file_id, references);
    pub fn bind_scope(&self, references);
    pub fn bind_edge_sources(&self, edges);
    pub fn bind_all(&self, file_id, references, edges);

    // 新增 P3 方法
    /// 建立词法绑定
    pub fn bind_lexical(&self, file_id: FileId, bindings: &mut Vec<BindingDef>, uses: &mut Vec<BindingUse>);
    /// 关联 binding 到 reference
    pub fn bind_references_to_bindings(&self, references: &mut [ReferenceUse], bindings: &[BindingDef]);
}
```

### DB 新增表

```sql
-- bindings: 词法绑定定义
CREATE TABLE bindings (
    binding_id BLOB PRIMARY KEY,
    file_id BLOB NOT NULL,
    function_id BLOB,
    scope_id BLOB NOT NULL,
    kind TEXT NOT NULL,           -- 'parameter' | 'local' | 'field' | 'import_alias' | 'catch_variable' | 'lambda_parameter' | 'global'
    name TEXT NOT NULL,
    symbol_id BLOB,
    range_start_byte INTEGER NOT NULL,
    range_end_byte INTEGER NOT NULL,
    range_start_line INTEGER NOT NULL,
    range_start_column INTEGER NOT NULL,
    range_end_line INTEGER NOT NULL,
    range_end_column INTEGER NOT NULL,
    FOREIGN KEY (file_id) REFERENCES files(file_id) ON DELETE CASCADE,
    FOREIGN KEY (function_id) REFERENCES symbols(symbol_id) ON DELETE SET NULL,
    FOREIGN KEY (scope_id) REFERENCES scopes(scope_id) ON DELETE CASCADE,
    FOREIGN KEY (symbol_id) REFERENCES symbols(symbol_id) ON DELETE SET NULL
);

-- binding_uses: 词法绑定使用点
CREATE TABLE binding_uses (
    binding_use_id BLOB PRIMARY KEY,
    file_id BLOB NOT NULL,
    scope_id BLOB NOT NULL,
    binding_id BLOB,
    reference_id BLOB,
    name TEXT NOT NULL,
    range_start_byte INTEGER NOT NULL,
    range_end_byte INTEGER NOT NULL,
    range_start_line INTEGER NOT NULL,
    range_start_column INTEGER NOT NULL,
    range_end_line INTEGER NOT NULL,
    range_end_column INTEGER NOT NULL,
    FOREIGN KEY (file_id) REFERENCES files(file_id) ON DELETE CASCADE,
    FOREIGN KEY (scope_id) REFERENCES scopes(scope_id) ON DELETE CASCADE,
    FOREIGN KEY (binding_id) REFERENCES bindings(binding_id) ON DELETE SET NULL,
    FOREIGN KEY (reference_id) REFERENCES "references"(reference_id) ON DELETE SET NULL
);

-- data_nodes: 数据流节点
CREATE TABLE data_nodes (
    data_node_id BLOB PRIMARY KEY,
    file_id BLOB NOT NULL,
    function_id BLOB,
    kind TEXT NOT NULL,           -- 'parameter' | 'local' | 'field' | 'return' | 'literal' | 'expr' | 'call_arg' | 'call_return' | 'receiver' | 'global' | 'unknown'
    binding_id BLOB,
    callsite_id BLOB,
    name TEXT,
    access_path TEXT,
    range_start_byte INTEGER NOT NULL,
    range_end_byte INTEGER NOT NULL,
    range_start_line INTEGER NOT NULL,
    range_start_column INTEGER NOT NULL,
    range_end_line INTEGER NOT NULL,
    range_end_column INTEGER NOT NULL,
    FOREIGN KEY (file_id) REFERENCES files(file_id) ON DELETE CASCADE,
    FOREIGN KEY (function_id) REFERENCES symbols(symbol_id) ON DELETE SET NULL,
    FOREIGN KEY (binding_id) REFERENCES bindings(binding_id) ON DELETE SET NULL,
    FOREIGN KEY (callsite_id) REFERENCES callsites(callsite_id) ON DELETE SET NULL
);

-- dataflow_edges: DataNode → DataNode
CREATE TABLE dataflow_edges (
    dataflow_edge_id BLOB PRIMARY KEY,
    source_node BLOB NOT NULL,
    target_node BLOB NOT NULL,
    kind TEXT NOT NULL,           -- 'assign' | 'read' | 'write' | 'field_load' | 'field_store' | 'arg_to_param' | 'return_to_call' | 'receiver_to_this' | 'phi' | 'sanitized'
    location_start_byte INTEGER NOT NULL,
    location_end_byte INTEGER NOT NULL,
    location_start_line INTEGER NOT NULL,
    location_start_column INTEGER NOT NULL,
    location_end_line INTEGER NOT NULL,
    location_end_column INTEGER NOT NULL,
    confidence REAL NOT NULL,
    FOREIGN KEY (source_node) REFERENCES data_nodes(data_node_id) ON DELETE CASCADE,
    FOREIGN KEY (target_node) REFERENCES data_nodes(data_node_id) ON DELETE CASCADE
);

-- callsite_args: callsite 实参
CREATE TABLE callsite_args (
    callsite_id BLOB NOT NULL,
    arg_index INTEGER NOT NULL,
    name TEXT,
    expr_text TEXT NOT NULL,
    data_node BLOB NOT NULL,
    range_start_byte INTEGER NOT NULL,
    range_end_byte INTEGER NOT NULL,
    range_start_line INTEGER NOT NULL,
    range_start_column INTEGER NOT NULL,
    range_end_line INTEGER NOT NULL,
    range_end_column INTEGER NOT NULL,
    PRIMARY KEY (callsite_id, arg_index),
    FOREIGN KEY (callsite_id) REFERENCES callsites(callsite_id) ON DELETE CASCADE,
    FOREIGN KEY (data_node) REFERENCES data_nodes(data_node_id) ON DELETE CASCADE
);
```

### DB 索引

```sql
-- binding 查询
CREATE INDEX idx_bindings_file ON bindings(file_id);
CREATE INDEX idx_bindings_function ON bindings(function_id);
CREATE INDEX idx_bindings_scope ON bindings(scope_id);

-- binding_use 查询
CREATE INDEX idx_binding_uses_file ON binding_uses(file_id);
CREATE INDEX idx_binding_uses_binding ON binding_uses(binding_id);

-- dataflow 查询
CREATE INDEX idx_data_nodes_file ON data_nodes(file_id);
CREATE INDEX idx_data_nodes_function ON data_nodes(function_id);
CREATE INDEX idx_data_nodes_binding ON data_nodes(binding_id);
CREATE INDEX idx_dataflow_edges_source ON dataflow_edges(source_node);
CREATE INDEX idx_dataflow_edges_target ON dataflow_edges(target_node);
```

### Schematic: 示例代码 → Binding + DataFlow

输入代码：
```ts
function handler(req) {
    const name = req.body.name;
    sink(name);
}
```

期望输出的 BindingDef：
```json
[
    { "id": "b1", "kind": "parameter", "name": "req", "function_id": "handler" },
    { "id": "b2", "kind": "local", "name": "name", "function_id": "handler" }
]
```

期望输出的 DataNode：
```text
dn1: parameter(req)
dn2: field(req.body)          access_path="req.body"
dn3: field(req.body.name)     access_path="req.body.name"
dn4: local(name)
dn5: call_arg(sink.arg0)
dn6: return(handler.return)
```

期望输出的 DataFlowEdge：
```text
dn1 → dn2  (ReqToField:  req → req.body)
dn2 → dn3  (FieldLoad:   req.body → req.body.name)
dn3 → dn4  (Assign:      req.body.name → name)
dn4 → dn5  (ArgFlow:     name → sink.arg0)
```

### 约束
- `edges` 表拆分为 `symbol_edges` (只保留 symbol-level: Contains/Calls/Extends/Implements/References/Imports/Exports/Overrides/Decorates)
- 旧的 `RawEdge` (dataflow kind: Parameter/Returns/Assigns/FieldRead/FieldWrite) 标记为 deprecated
- DataNodeId → DataNodeId，不再用 SymbolId 表达 dataflow node
- BindingGraph 和 DataFlowGraph 按需加载，不全量 snapshot
- SemanticBinder 是本阶段的唯一 binder 权威

### 验收
```ts
function f(req) {
    const name = req.body.name;
    return name;
}
```
必须能产出：
- binding: parameter(req), local(name)
- dataflow: req → req.body → req.body.name → name → return

---

## P4：CFG 与跨函数 dataflow

### 目标
支持 interprocedural dataflow，通过函数摘要实现跨函数传播。

### 模块结构

```
src/
  types/
    cfg.rs                ← 新增：CfgNode, CfgEdge, CfgNodeId, CfgEdgeId, CfgNodeKind, CfgEdgeKind
    dataflow.rs           ← 扩展：FunctionSummary, SummaryFlow
    mod.rs                ← 更新导出
  extraction/
    cfg_builder.rs        ← 新增：基于 AST 构建函数内 CFG
    extract.rs            ← 更新：调用 CfgBuilder
  analysis/
    mod.rs                ← 新增模块
    dataflow/
      mod.rs              ← 新增
      intraprocedural.rs  ← 函数内 dataflow 传播引擎
      interprocedural.rs  ← 跨函数 dataflow 传播引擎
      summaries.rs        ← FunctionSummary 构建与存储
      worklist.rs         ← SCC/worklist 分析框架
  db/
    schema.rs             ← version bump, 新增 cfg_nodes, cfg_edges, function_summaries
    store.rs              ← 新增 API
  graph/
    cfg.rs                ← 新增：CFG 按函数加载
```

### 新增类型

```rust
// ================================================================
// src/types/cfg.rs
// ================================================================

pub struct CfgNodeId([u8; 32]);
impl CfgNodeId {
    pub fn generate(function_id, kind, start_byte) -> Self;
}

pub struct CfgEdgeId([u8; 32]);
impl CfgEdgeId {
    pub fn generate(source, target, kind) -> Self;
}

pub struct CfgNode {
    pub id: CfgNodeId,
    pub function_id: SymbolId,
    pub kind: CfgNodeKind,
    pub stmt_range: TextRange,
}

pub enum CfgNodeKind {
    Entry,
    Exit,
    Statement,
    Branch,
    Loop,
    Return,
    Throw,
    Join,
}

pub struct CfgEdge {
    pub id: CfgEdgeId,
    pub source: CfgNodeId,
    pub target: CfgNodeId,
    pub kind: CfgEdgeKind,
}

pub enum CfgEdgeKind {
    Normal,
    TrueBranch,
    FalseBranch,
    LoopBack,
    Exception,
}

// ================================================================
// src/types/dataflow.rs 扩展
// ================================================================

/// 函数摘要 — 跨函数 dataflow 的核心
pub struct FunctionSummary {
    pub function_id: SymbolId,
    pub input_flows: Vec<SummaryFlow>,     // param → output
    pub sources: Vec<SummarySource>,
    pub sinks: Vec<SummarySink>,
    pub sanitizers: Vec<SummarySanitizer>,
    pub side_effects: Vec<SummaryEffect>,
}

pub struct SummaryFlow {
    pub from_access_path: String,   // e.g. "param0.query.name"
    pub to_node: SummaryNodeRef,    // e.g. return, param1, field_X
    pub kind: DataFlowKind,
}

pub enum SummaryNodeRef {
    Return,
    Parameter(u32),
    Field(String),
}
```

**示例摘要：**
```ts
function getName(req) {
    return req.query.name;
}
```
摘要：
```json
{
    "function_id": "getName",
    "input_flows": [
        { "from_access_path": "param0.query.name", "to_node": "Return" }
    ]
}
```

### 核心组件

#### 1. CfgBuilder (src/extraction/cfg_builder.rs)

```
输入：tree-sitter AST + scope tree
输出：Vec<CfgNode> + Vec<CfgEdge>

初期支持：
  - block → Statement nodes + Normal edges
  - if/else → Branch node + TrueBranch/FalseBranch + Join
  - for/while → Loop node + LoopBack
  - return → Return node
  - throw → Throw node

不做：try/catch/finally, switch, async/await (后续迭代)
```

#### 2. IntraproceduralDataflow (src/analysis/dataflow/intraprocedural.rs)

```
输入：per-function DataNode + DataFlowEdge + CFG
输出：函数内的 def-use chain + taint propagation

算法：worklist-based forward dataflow
  - 从参 DataNode 开始
  - 沿 DataFlowEdge 传播
  - 遇到 callsite 时暂存，等待 interprocedural 连接

输出：每个 DataNode 的 ReachableFacts
```

#### 3. InterproceduralDataflow (src/analysis/dataflow/interprocedural.rs)

```
输入：CallGraph + FunctionSummary[] + 跨函数 edge candidates
输出：跨函数传播后的完整 dataflow

算法：SCC-based worklist
  1. SCC 拓扑排序
  2. 每个 SCC 内做 fixpoint iteration
  3. 遇到 callee 有 summary 时：
     - 把 arg access_path 映射到 summary 的 param0 access_path
     - 把 summary 的 return 映射到 call return DataNode
  4. 遇到 unknown callee：保守假设 (taint 传播到所有可达节点)

关键连接：
  callsite.arg[i] → callee.param[i]    (ArgToParam edge)
  callee.return → callsite.return      (ReturnToCall edge)
  this/receiver → callee.this           (ReceiverToThis edge)
```

#### 4. FunctionSummary (src/analysis/dataflow/summaries.rs)

```
构建策略：
  1. 对每个函数，intraprocedural 分析完成后提取 summary
  2. summary 编码 param access_path → return/output 的关系
  3. 存储到 function_summaries 表
  4. interprocedural 阶段消费 summary

更新策略：
  - 函数体修改 → 重新分析 → 更新 summary
  - 依赖的函数 summary 更新 → 级联重新分析 (SCC/worklist)
```

### DB 新增表

```sql
-- cfg_nodes: 控制流图节点
CREATE TABLE cfg_nodes (
    cfg_node_id BLOB PRIMARY KEY,
    function_id BLOB NOT NULL,
    kind TEXT NOT NULL,              -- entry/exit/statement/branch/loop/return/throw/join
    range_start_byte INTEGER NOT NULL,
    range_end_byte INTEGER NOT NULL,
    range_start_line INTEGER NOT NULL,
    range_start_column INTEGER NOT NULL,
    range_end_line INTEGER NOT NULL,
    range_end_column INTEGER NOT NULL,
    FOREIGN KEY (function_id) REFERENCES symbols(symbol_id) ON DELETE CASCADE
);

-- cfg_edges: 控制流边
CREATE TABLE cfg_edges (
    cfg_edge_id BLOB PRIMARY KEY,
    source_node BLOB NOT NULL,
    target_node BLOB NOT NULL,
    kind TEXT NOT NULL,              -- normal/true_branch/false_branch/loop_back/exception
    FOREIGN KEY (source_node) REFERENCES cfg_nodes(cfg_node_id) ON DELETE CASCADE,
    FOREIGN KEY (target_node) REFERENCES cfg_nodes(cfg_node_id) ON DELETE CASCADE
);

-- function_summaries: 函数摘要
CREATE TABLE function_summaries (
    function_id BLOB PRIMARY KEY,
    summary_json TEXT NOT NULL,      -- FunctionSummary → JSON
    confidence REAL NOT NULL,
    computed_at TEXT NOT NULL,       -- timestamp
    FOREIGN KEY (function_id) REFERENCES symbols(symbol_id) ON DELETE CASCADE
);
```

### 验收

跨函数追踪：
```ts
function getName(req) { return req.query.name; }
function handler(req) {
    const x = getName(req);
    sink(x);
}
```
必须能产出：
```text
req.query.name → getName.return → x → sink.arg0
```

---

## P5：污点分析 MVP

### 目标
实现可解释的 source-to-sink 分析。

### 模块结构

```
src/
  types/
    taint.rs             ← 新增：TaintRule, TaintFinding, TaintPathStep, TaintFindingId
    mod.rs               ← 更新导出
  analysis/
    taint/
      mod.rs             ← 新增
      rules.rs           ← 规则加载器 (YAML/TOML/JSON)
      engine.rs          ← 污点传播引擎
      path.rs            ← 路径追踪 (source → ... → sink)
      findings.rs        ← finding 存储与查询
  db/
    schema.rs            ← version bump, 新增 taint_rules, taint_findings, taint_path_steps
    store.rs             ← 新增 API
  cli/commands/
    taint.rs             ← 新增：atlas taint 子命令
  mcp/
    tools.rs             ← 新增：atlas_taint_findings, atlas_taint_path tools
```

### 新增类型

```rust
// ================================================================
// src/types/taint.rs
// ================================================================

pub struct TaintFindingId([u8; 32]);

pub struct TaintRule {
    pub id: String,
    pub language: Option<Language>,
    pub kind: TaintRuleKind,
    pub symbol_pattern: String,
    pub access_path_pattern: Option<String>,
    pub argument_index: Option<u32>,
    pub applies_to_return: bool,
}

pub enum TaintRuleKind {
    Source,        // 数据来源 (e.g. req.body, request.args)
    Sink,          // 危险汇点 (e.g. exec, SQL query)
    Sanitizer,     // 净化函数 (e.g. escapeHtml)
    Propagator,    // 传播函数 (e.g. JSON.parse)
}

pub struct TaintFinding {
    pub id: TaintFindingId,
    pub source_node: DataNodeId,
    pub sink_node: DataNodeId,
    pub rule_id: String,
    pub severity: Severity,
    pub confidence: Confidence,
}

pub struct TaintPathStep {
    pub finding_id: TaintFindingId,
    pub index: u32,
    pub data_node: DataNodeId,
    pub edge_id: Option<DataFlowEdgeId>,
    pub file_id: FileId,
    pub range: TextRange,
    pub message: String,
}
```

### 核心组件

#### 1. TaintRuleLoader (src/analysis/taint/rules.rs)

```
支持格式：YAML / TOML / JSON
规则文件路径：.atlas/rules/*.yaml

加载流程：
  1. 扫描 .atlas/rules/ 目录
  2. 解析每个规则文件
  3. 按 language 索引
  4. 返回 Vec<TaintRule>
```

默认规则示例 (YAML)：
```yaml
# .atlas/rules/default.yaml
sources:
  - id: express.req.query
    language: typescript
    kind: source
    symbol_pattern: "Request"
    access_path_pattern: "*.query.*"
  - id: express.req.body
    language: typescript
    kind: source
    symbol_pattern: "Request"
    access_path_pattern: "*.body.*"
  - id: python.flask.request.args
    language: python
    kind: source
    symbol_pattern: "request.args"

sinks:
  - id: node.child_process.exec
    language: typescript
    kind: sink
    callee: "child_process.exec"
    argument_index: 0
  - id: python.os.system
    language: python
    kind: sink
    callee: "os.system"
    argument_index: 0
  - id: sql.query
    language: typescript
    kind: sink
    callee: "*.query"
    argument_index: 0

sanitizers:
  - id: escape.html
    kind: sanitizer
    callee: "escapeHtml"
    applies_to_return: true
```

#### 2. TaintEngine (src/analysis/taint/engine.rs)

```
算法：source → forward propagation → sink check

步骤：
  1. Load taint rules
  2. Mark source DataNodes:
     - 匹配 rule.symbol_pattern → 找到 Symbol
     - 匹配 rule.access_path_pattern → 找到 DataNode
     - 标记为 tainted
  3. Forward intraprocedural propagation:
     - worklist: 对每个 tainted DataNode
     - 沿 DataFlowEdge 传播 taint
     - 对每个 callsite:
       a. 查 callee summary
       b. arg → param 映射
       c. callee summary return → call return
  4. Check sink DataNodes:
     - 匹配 rule.symbol_pattern → 找到 sink symbol
     - 匹配 rule.argument_index → 找到对应的 call argument DataNode
     - 如果该 DataNode tainted → 产生 finding
  5. Emit finding path = trace(source → ... → sink)
```

#### 3. TaintPathTracer (src/analysis/taint/path.rs)

```
输入：TaintFinding (source_node, sink_node)
输出：Vec<TaintPathStep>

算法：从 sink_node 反向 BFS 沿 DataFlowEdge 到 source_node
  - 记录每一步的 DataNode, DataFlowEdge, file_id, range
  - 生成人类可读的 message (e.g. "req.query → req.query.name → name → sink")
```

### DB 新增表

```sql
-- taint_rules: 污点规则
CREATE TABLE taint_rules (
    rule_id TEXT PRIMARY KEY,
    language TEXT,
    kind TEXT NOT NULL,
    symbol_pattern TEXT NOT NULL,
    access_path_pattern TEXT,
    argument_index INTEGER,
    applies_to_return INTEGER DEFAULT 0
);

-- taint_findings: 污点发现
CREATE TABLE taint_findings (
    finding_id BLOB PRIMARY KEY,
    source_node BLOB NOT NULL,
    sink_node BLOB NOT NULL,
    rule_id TEXT NOT NULL,
    severity TEXT NOT NULL,
    confidence REAL NOT NULL,
    FOREIGN KEY (source_node) REFERENCES data_nodes(data_node_id),
    FOREIGN KEY (sink_node) REFERENCES data_nodes(data_node_id),
    FOREIGN KEY (rule_id) REFERENCES taint_rules(rule_id)
);

-- taint_path_steps: finding 路径步骤
CREATE TABLE taint_path_steps (
    finding_id BLOB NOT NULL,
    step_index INTEGER NOT NULL,
    data_node BLOB NOT NULL,
    edge_id BLOB,
    file_id BLOB NOT NULL,
    range_start_byte INTEGER NOT NULL,
    range_end_byte INTEGER NOT NULL,
    range_start_line INTEGER NOT NULL,
    range_start_column INTEGER NOT NULL,
    range_end_line INTEGER NOT NULL,
    range_end_column INTEGER NOT NULL,
    message TEXT NOT NULL,
    PRIMARY KEY (finding_id, step_index),
    FOREIGN KEY (finding_id) REFERENCES taint_findings(finding_id) ON DELETE CASCADE,
    FOREIGN KEY (data_node) REFERENCES data_nodes(data_node_id),
    FOREIGN KEY (file_id) REFERENCES files(file_id)
);
```

### MCP 工具新增

```yaml
tools:
  - name: atlas_taint_findings
    description: Get taint analysis findings
    parameters:
      language: optional filter by language
      severity: optional filter by severity
      limit: max results

  - name: atlas_taint_path
    description: Get detailed taint path for a finding
    parameters:
      finding_id: the finding's ID
```

### CLI 新增

```bash
atlas taint             # 运行污点分析
atlas taint --language typescript
atlas taint --rules .atlas/rules/custom.yaml
atlas taint find <id>   # 查看 finding 路径详情
```

### 默认规则覆盖范围

P5 MVP 应内置以下默认规则：

**TypeScript/JavaScript:**
- Sources: `req.query`, `req.body`, `req.params`, `req.headers`, `window.location`
- Sinks: `child_process.exec`, `eval`, `document.write`, `innerHTML`, SQL queries
- Sanitizers: `escapeHtml`, `encodeURIComponent`, `DOMPurify.sanitize`

**Python:**
- Sources: `request.args`, `request.json`, `request.form`, `sys.argv`
- Sinks: `os.system`, `subprocess.call`, `eval`, `exec`, `sqlite3.execute`
- Sanitizers: `html.escape`, `json.dumps`

### 约束
- 不修改 BindingGraph/DataFlowGraph 的语义
- 不新增额外 AST 解析
- 规则可扩展，用户可自定义

### 验收
```ts
function handler(req) {
    const name = req.query.name;
    conn.query("SELECT * FROM users WHERE name = " + name);
}
```
必须能输出：
```text
Finding: express.req.query → sql.injection (severity: high, confidence: 0.95)
Path:
  1. req.query (source) at handler.ts:2
  2. req.query → req.query.name (field access) at handler.ts:2
  3. req.query.name → name (assign) at handler.ts:2
  4. name → conn.query.arg0 (call arg) at handler.ts:3
  5. conn.query.arg0 (sink) at handler.ts:3
```

---

## 完整模块关系图

```
语言源码
  │
  ▼
┌──────────────────┐     P1: ParseWorkerPool + timeout + 错误报告
│   extraction/    │
│   ├─ extract.rs  │ ←── P0: SemanticBinder (source/scope/binding)
│   ├─ worker.rs   │ ←── P1: 进程隔离
│   └─ languages/  │
└──────┬───────────┘
       │ FileFacts
       ▼
┌──────────────────┐     P1: file lock + index report
│      db/         │     P2: resolved invalidation
│   ├─ store.rs    │     P3: bindings/dataflow_edges/callsite_args 写入
│   └─ schema.rs   │     P4: cfg/function_summaries 写入
└──────┬───────────┘     P5: taint_rules/findings/path_steps 写入
       │
       ▼
┌──────────────────┐     P2: import/export/path-alias/package resolver
│   resolution/    │     P2: Resolver → GraphBuilder 分离
│   ├─ mod.rs      │     P2: resolved fact invalidation
│   ├─ graph_builder.rs  P3: call_edges
│   └─ ...           │
└──────┬───────────┘
       │
       ▼
┌──────────────────┐     P3: BindingGraph + DataFlowGraph
│     graph/       │     P4: CFG
│   ├─ symbol_graph.rs │ P3: symbol_edges (deprecate dataflow in edges)
│   ├─ binding_graph.rs
│   ├─ dataflow_graph.rs
│   └─ cfg.rs
└──────┬───────────┘
       │
       ▼
┌──────────────────┐     P1: query parser
│     search/      │     P2-P5: 扩展 search (taint/member)
│    context/      │
│      mcp/        │     P5: atlas_taint_findings, atlas_taint_path
└──────┬───────────┘
       │
       ▼
┌──────────────────┐     P4: intraprocedural + interprocedural dataflow
│    analysis/     │     P5: taint engine
│   ├─ dataflow/   │
│   └─ taint/      │
└──────────────────┘
```

---

## 实施优先级与依赖

```
P1 (可并行启动)
  ├── ParseWorkerPool
  ├── FileLock
  ├── IndexReport
  ├── SearchQueryParser
  └── GoldenTestFramework

P2 (依赖 P1 稳定)
  ├── Resolver/GraphBuilder 分离
  ├── Resolved Fact Invalidation
  ├── Import Resolution 增强
  └── callsite.callee 回填

P3 (依赖 P2 call graph 稳定)
  ├── BindingDef/BindingUse + DB
  ├── DataNode/DataFlowEdge + DB
  ├── LexicalBinder + DataFlowBuilder
  ├── edges → symbol_edges 拆分
  └── CallsiteArgs 落库

P4 (依赖 P3 DataFlowGraph)
  ├── CFG + DB
  ├── Intraprocedural dataflow 引擎
  ├── Interprocedural dataflow 引擎
  └── FunctionSummary

P5 (依赖 P4 interprocedural)
  ├── Taint rules
  ├── Taint engine
  ├── Finding path
  └── MCP/CLI 输出
```

---

## 全局约束总结

| 约束 | 说明 |
|------|------|
| Adapter 不填 source_symbol/scope_id | P0 已实现 |
| SemanticBinder 是单一权威 | P0 已实现，P3 扩展 |
| ReferenceId 必须含 kind | P0 已实现 |
| References 永不删除 | P0 已保持 |
| Resolver 不创建 edges | P2 实现 |
| DataFlow 不用 SymbolId 伪装 | P3 实现 |
| Graph 分层加载 | P3/P4 实现 |
| 每个阶段有 golden tests | P1 起持续 |
| 不允许静默吞错误 | 所有 diagnostics 必须记录 |
