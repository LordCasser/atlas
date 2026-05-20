# Atlas v2 全量干净重构方案：从符号索引器到多语言语义分析平台

> 面向后续开发 Agent 的重构实施文档。  
> 本文描述 Atlas 面向未来最优架构时，如何进行全量、干净、分层的 v2 重构。  
> 目标不是简单复制 CodeGraph，而是在追平 CodeGraph 产品化能力的基础上，演化为可承载污点分析、跨函数数据流、影响分析和 Agent 可解释推理的本地语义分析平台。

---

## 0. 总体结论

当前 Atlas 的核心问题不是某个语言 `.scm` 写得不够好，而是多个语义层被混在一起：

```text
.scm query
  + adapter normalize
  + SymbolId generation
  + source ownership guessing
  + RawEdge graph construction
  + partial dataflow
  + resolution edge creation
```

这导致：

- definitions 与 references/dataflow 的 ID 生成路径分裂。
- `RawEdge` 同时承载 symbol graph 和弱 dataflow。
- `SymbolId` 被用于真实 symbol，也被用于虚拟 dataflow target。
- `ReferenceUse.scope_id` 与 lexical binding 不完整。
- callsite 缺少 args/return/receiver 的结构化事实。
- 无法自然支持污点分析、跨函数数据流、CFG、summary。

v2 重构的第一性原则：

```text
Parse Tree
  ↓
Language Frontend
  ↓
HIR / Normalized AST
  ↓
Semantic Binder
  ↓
Typed Fact Graphs
  - SymbolGraph
  - BindingGraph
  - CallGraph
  - DataFlowGraph
  - CFG
  ↓
Project Resolution
  ↓
Analysis Engines
  - Search
  - Context
  - Impact
  - Taint
  - Cross-function Dataflow
```

关键设计决策：

1. Frontend 不直接生成最终语义边。
2. Adapter 不再猜最终 `source_symbol`。
3. `SymbolDef`、`BindingDef`、`ReferenceUse`、`DataNode` 必须分离。
4. Dataflow 不再使用 `RawEdge<SymbolId, SymbolId>`。
5. Graph 必须分层，不再使用一个万能 `edges` 表表达所有关系。
6. Resolver 只负责 resolution，不直接创建 graph edges。
7. 高阶分析建立在 BindingGraph + DataFlowGraph + CFG + FunctionSummary 上。

---

## 1. 重构目标

### 1.1 产品目标

Atlas v2 需要成为一个可被开发者和 Agent 可靠使用的本地语义平台：

- 快速 index 大型仓库。
- 大项目中单个坏文件不会拖垮整个 index。
- 具备 parse timeout、worker recycle、retry、max file size、index report。
- 支持 agent-friendly search query，例如：

```text
kind:function lang:typescript path:src name:auth authenticate
```

- 支持 MCP 工具稳定查询 symbol、reference、call graph、dataflow、taint finding。
- 在易用性和鲁棒性上追平 CodeGraph。

### 1.2 分析目标

Atlas v2 还必须支持比 CodeGraph 更深入的程序分析：

- 精确 lexical binding / def-use。
- expression-level dataflow。
- callsite args/return/receiver。
- CFG。
- function summary。
- interprocedural dataflow。
- taint source/sink/sanitizer 分析。
- 可解释 taint path。
- impact analysis / slicing。

---

## 2. 当前 v1 架构问题

### 2.1 当前 v1 pipeline

```text
tree-sitter parse
  ↓
definitions.scm -> SymbolDef
references.scm  -> ReferenceUse
imports.scm     -> ImportDef
scopes.scm      -> ScopeDef
dataflow.scm    -> RawEdge
  ↓
build_scope_tree
  ↓
SymbolRegistry 修 source
  ↓
Store
  ↓
ReferenceResolver
  ↓
GraphSnapshot
```

### 2.2 主要问题

#### 问题 1：语义阶段过早生成最终 ID

当前 adapter 在 normalize 阶段直接生成：

- `SymbolId`
- `ReferenceId`
- `EdgeId`
- `ImportId`

这导致后续 binder/resolver 发现信息变化后必须重写 ID。

v2 方向：

- Frontend 只生成 raw node/hir node 临时 ID。
- SemanticBinder 确定 scope/binding/source 后再生成稳定语义 ID。

#### 问题 2：Adapter 职责过重

当前 adapter 同时负责：

- 解释 capture。
- 判断 symbol kind。
- 猜 qualified name。
- 猜 source owner。
- 猜 dataflow source。
- 生成最终 ID。

v2 方向：

- Language Frontend 只负责语言语法 lowering。
- SemanticBinder 负责 owner/scope/binding。
- Resolver 负责跨文件解析。
- GraphBuilder 负责 graph edge。

#### 问题 3：`RawEdge` 语义混乱

当前 `RawEdge` 同时承载：

- calls
- references
- contains
- imports
- parameter
- returns
- assigns
- field read/write

但 dataflow target 经常是虚拟 `SymbolId`，不是实际 symbol。

v2 方向：

- `symbol_edges` 表示 symbol -> symbol。
- `call_edges` 表示 callsite/caller -> callee。
- `dataflow_edges` 表示 data node -> data node。
- `cfg_edges` 表示 cfg node -> cfg node。

#### 问题 4：缺少 BindingGraph

当前局部变量、参数、import alias、catch var 等没有统一 binding 模型。

后果：

- `ReferenceUse.scope_id` 弱。
- local def-use 弱。
- taint source 无法可靠传播到 local use。

v2 方向：

- 新增 `BindingDef` / `BindingUse`。
- 区分 project-level symbol 与 lexical binding。

#### 问题 5：缺少 expression-level HIR

当前 `.scm` 可以找到一些 definitions/references，但很难精确表达：

- assignment lhs/rhs
- nested member access
- chained calls
- call args
- return expression
- destructuring
- lambda
- branch/loop

v2 方向：

- 语言前端降低到统一 HIR。
- DataFlowBuilder 基于 HIR 构建数据流。

#### 问题 6：Resolver 与 EdgeBuilder 混合

当前 `ReferenceResolver::create_edges()` 在 resolution 期间直接创建 edges。

v2 方向：

```text
Resolver -> resolved targets / callsite callee
GraphBuilder -> 根据 resolved facts 构建 graph edges
```

---

## 3. v2 目标架构

推荐最终目录结构：

```text
src/
  frontend/
    mod.rs
    parser.rs
    raw.rs
    lower.rs
    language.rs
    tree_sitter/
    languages/
      mod.rs
      typescript.rs
      javascript.rs
      python.rs
      java.rs
      c.rs
      cpp.rs
      arkts.rs
      cangjie.rs

  hir/
    mod.rs
    ids.rs
    file.rs
    item.rs
    stmt.rs
    expr.rs
    scope.rs
    visitor.rs

  semantic/
    mod.rs
    binder.rs
    scope_index.rs
    symbol_table.rs
    binding_table.rs
    reference_binder.rs
    import_export_binder.rs
    member_binder.rs
    callsite_binder.rs
    diagnostics.rs

  facts/
    mod.rs
    raw_file_facts.rs
    bound_file_facts.rs
    graph_facts.rs

  graph/
    mod.rs
    symbol_graph.rs
    call_graph.rs
    binding_graph.rs
    dataflow_graph.rs
    cfg.rs
    traversal.rs
    graph_store.rs

  analysis/
    mod.rs
    search/
    context/
    impact/
    dataflow/
      mod.rs
      worklist.rs
      summaries.rs
    taint/
      mod.rs
      rules.rs
      engine.rs
      path.rs
      findings.rs

  db/
    schema.rs
    store.rs
    reader.rs
    writer.rs
    migrations/
      v1.sql
      v2.sql
      v3.sql

  cli/
  mcp/
  sync/
  types/
```

---

## 4. v2 Pipeline 设计

### 4.1 完整 pipeline

```text
1. Discovery
   - git-aware file discovery
   - .atlasignore
   - include/exclude
   - max file size

2. Parse
   - tree-sitter parse
   - timeout
   - worker isolation
   - retry/recycle

3. Frontend Lowering
   - parse tree -> HIR / RawFileFacts
   - language-specific syntax normalization

4. Semantic Binding
   - scope tree
   - symbol table
   - lexical bindings
   - references
   - callsites
   - imports/exports

5. Local Graph Construction
   - symbol facts
   - binding facts
   - callsite facts
   - dataflow facts
   - cfg facts

6. Store
   - per-file bound facts persisted
   - diagnostics persisted

7. Project Resolution
   - import/export
   - type/member
   - call target
   - reference target

8. Global Graph Build / Update
   - symbol_edges
   - call_edges
   - dataflow interprocedural edges
   - summaries

9. Analysis Engines
   - search/context
   - impact
   - taint
   - dataflow queries
```

### 4.2 关键原则

- Frontend 不做跨文件 resolution。
- SemanticBinder 不做 fuzzy/project-wide resolution。
- Resolver 不直接创建 graph edges。
- GraphBuilder 不重新解析源码。
- AnalysisEngine 不直接猜语法事实，只消费 typed facts。

---

## 5. Frontend 设计

### 5.1 `.scm` 与 AST walker 的定位

v2 不应完全依赖 `.scm`。

推荐混合方式：

```text
.scm query:
  - 快速定位 top-level definitions/imports/scopes
  - 适合作为入口和补充

AST walker:
  - expression lowering
  - statement lowering
  - call args
  - assignment lhs/rhs
  - control flow
  - destructuring/patterns
  - nested member access
```

### 5.2 Frontend 输出

Frontend 输出 HIR 或 RawFileFacts，而不是最终 DB facts。

```rust
pub struct RawFileFacts {
    pub file: FileInfo,
    pub items: Vec<RawItem>,
    pub scopes: Vec<RawScope>,
    pub imports: Vec<RawImport>,
    pub exports: Vec<RawExport>,
    pub statements: Vec<RawStatement>,
    pub expressions: Vec<RawExpression>,
    pub diagnostics: Vec<Diagnostic>,
}
```

这些 raw facts 使用临时 ID：

```rust
RawNodeId
RawScopeId
RawStmtId
RawExprId
```

不要在该阶段生成最终 `SymbolId` / `ReferenceId` / `EdgeId`。

### 5.3 首批语言建议

v2 首先支持：

1. TypeScript / JavaScript / ArkTS 共享 frontend。
2. Python。

原因：

- TS/JS 是 CodeGraph 对标重点。
- Python 是安全/污点分析重点。
- 两者语法差异足以验证抽象。

Java/C/C++/Cangjie 在 v2 core 稳定后迁移。

---

## 6. HIR 设计

HIR 是跨语言统一的高层中间表示。

### 6.1 HirFile

```rust
pub struct HirFile {
    pub file_id: FileId,
    pub language: Language,
    pub module_path: Option<ModulePath>,
    pub items: Vec<HirItem>,
    pub scopes: Vec<HirScope>,
    pub imports: Vec<HirImport>,
    pub exports: Vec<HirExport>,
    pub statements: Vec<HirStmt>,
    pub expressions: Vec<HirExpr>,
    pub diagnostics: Vec<Diagnostic>,
}
```

### 6.2 HirItem

```rust
pub struct HirItem {
    pub id: HirItemId,
    pub kind: HirItemKind,
    pub name: Option<String>,
    pub parent: Option<HirItemId>,
    pub scope: Option<HirScopeId>,
    pub range: TextRange,
    pub name_range: Option<TextRange>,
    pub signature: Option<String>,
    pub modifiers: ItemModifiers,
}
```

```rust
pub enum HirItemKind {
    Module,
    Namespace,
    Package,
    Class,
    Struct,
    Interface,
    Trait,
    Enum,
    EnumMember,
    Function,
    Method,
    Constructor,
    Field,
    Property,
    Variable,
    Constant,
    TypeAlias,
    Macro,
}
```

### 6.3 HirStmt

```rust
pub enum HirStmtKind {
    Block { statements: Vec<HirStmtId> },
    Let { bindings: Vec<HirPatternId>, value: Option<HirExprId> },
    Expr { expr: HirExprId },
    Return { value: Option<HirExprId> },
    If { condition: HirExprId, then_branch: HirStmtId, else_branch: Option<HirStmtId> },
    Loop { body: HirStmtId },
    For { pattern: Option<HirPatternId>, iterable: Option<HirExprId>, body: HirStmtId },
    While { condition: HirExprId, body: HirStmtId },
    Try,
    Throw { value: Option<HirExprId> },
    Unknown,
}
```

### 6.4 HirExpr

```rust
pub enum HirExprKind {
    Identifier { name: String },
    Literal { text: String },
    MemberAccess { receiver: HirExprId, member: String },
    Call { callee: HirExprId, args: Vec<HirArg> },
    New { callee: HirExprId, args: Vec<HirArg> },
    Assignment { lhs: HirExprId, rhs: HirExprId },
    Binary { lhs: HirExprId, rhs: HirExprId, op: String },
    Unary { expr: HirExprId, op: String },
    ReturnValue { value: Option<HirExprId> },
    ObjectLiteral,
    ArrayLiteral,
    Lambda { params: Vec<HirPatternId>, body: HirStmtId },
    Await { expr: HirExprId },
    Unknown,
}
```

### 6.5 HirPattern

必须支持 destructuring / pattern binding：

```rust
pub enum HirPatternKind {
    Identifier { name: String },
    Object { fields: Vec<HirPatternId> },
    Array { elements: Vec<HirPatternId> },
    Rest { inner: HirPatternId },
    Unknown,
}
```

---

## 7. SemanticBinder v2

### 7.1 输入输出

输入：

```rust
HirFile
```

输出：

```rust
pub struct BoundFileFacts {
    pub file: FileInfo,

    pub symbols: Vec<SymbolDef>,
    pub scopes: Vec<ScopeDef>,

    pub bindings: Vec<BindingDef>,
    pub binding_uses: Vec<BindingUse>,

    pub references: Vec<ReferenceUse>,
    pub imports: Vec<ImportDef>,
    pub exports: Vec<ExportDef>,

    pub callsites: Vec<Callsite>,
    pub callsite_args: Vec<CallsiteArg>,

    pub data_nodes: Vec<DataNode>,
    pub dataflow_edges: Vec<DataFlowEdge>,

    pub cfg_nodes: Vec<CfgNode>,
    pub cfg_edges: Vec<CfgEdge>,

    pub diagnostics: Vec<Diagnostic>,
}
```

### 7.2 Binder 子组件

```text
SemanticBinder
  ├── ScopeBinder
  ├── SymbolBinder
  ├── LexicalBinder
  ├── ReferenceBinder
  ├── ImportExportBinder
  ├── MemberBinder
  ├── CallsiteBinder
  ├── DataFlowBuilder
  └── CfgBuilder
```

### 7.3 ScopeBinder

职责：

- 构建 scope tree。
- 为 item/stmt/expr 查找 innermost scope。
- 建立 parent/children/ancestor 查询。

核心 API：

```rust
pub struct ScopeIndex {
    scopes: Vec<ScopeDef>,
    parents: HashMap<ScopeId, ScopeId>,
    children: HashMap<ScopeId, Vec<ScopeId>>,
}

impl ScopeIndex {
    pub fn innermost_scope(&self, range: TextRange) -> Option<ScopeId>;
    pub fn parent(&self, scope: ScopeId) -> Option<ScopeId>;
    pub fn ancestors(&self, scope: ScopeId) -> impl Iterator<Item = ScopeId>;
}
```

### 7.4 SymbolBinder

职责：

- 将 HIR item 变成 `SymbolDef`。
- 生成稳定 `SymbolId`。
- 建立 owner_by_scope。
- 确定 container。
- 确定 qualified_name。

注意：

- local variable 不应默认变成 `SymbolDef`。
- function/class/method/module/field/property/type 等才是主要 Symbol。

### 7.5 LexicalBinder

职责：

- 建立 `BindingDef`。
- 建立 `BindingUse`。
- 处理 shadowing。
- 处理参数、局部变量、import alias、catch var、lambda params、destructuring。

### 7.6 ReferenceBinder

职责：

- 为 identifier/member/call/type usage 生成 `ReferenceUse`。
- 填充：
  - `scope_id`
  - `source_symbol`
  - `binding_id`
  - `receiver`
  - `arity`
  - `kind`

### 7.7 CallsiteBinder

职责：

- 基于 HIR call/new expression 生成 `Callsite`。
- 生成 `CallsiteArg`。
- 绑定 caller。
- 暂不要求在本阶段确定 callee；callee 由 Project Resolver 后续回填。

### 7.8 DataFlowBuilder

职责：

- 基于 HIR + Binding 构建函数内 data nodes 和 dataflow edges。
- 不做跨函数传播。
- 跨函数边由 interprocedural dataflow / summary 阶段补充。

### 7.9 CfgBuilder

职责：

- 基于 HIR statement 构建函数内 CFG。
- 初期支持：block、if、loop、return、throw。
- 后续支持 try/catch/finally、switch、async/await。

---

## 8. Symbol / Binding / Reference 分离

### 8.1 SymbolDef

表示项目级或文件级重要实体：

```text
module
namespace
class
struct
interface
enum
function
method
constructor
field
property
type alias
macro
```

适合：

- 搜索
- go-to-definition
- import/export
- call graph
- public API 分析

### 8.2 BindingDef

表示词法绑定：

```text
parameter
local variable
catch variable
import alias
destructuring binding
lambda parameter
temporary binding
```

建议结构：

```rust
pub struct BindingDef {
    pub id: BindingId,
    pub file_id: FileId,
    pub function_id: Option<SymbolId>,
    pub scope_id: ScopeId,
    pub kind: BindingKind,
    pub name: String,
    pub symbol_id: Option<SymbolId>,
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
    Temporary,
}
```

### 8.3 BindingUse

```rust
pub struct BindingUse {
    pub id: BindingUseId,
    pub file_id: FileId,
    pub scope_id: ScopeId,
    pub binding_id: Option<BindingId>,
    pub reference_id: Option<ReferenceId>,
    pub name: String,
    pub range: TextRange,
}
```

### 8.4 ReferenceUse

```rust
pub struct ReferenceUse {
    pub id: ReferenceId,
    pub file_id: FileId,
    pub source_symbol: Option<SymbolId>,
    pub scope_id: Option<ScopeId>,
    pub binding_id: Option<BindingId>,
    pub kind: ReferenceKind,
    pub text: String,
    pub name: String,
    pub receiver_expr: Option<HirExprId>,
    pub receiver_text: Option<String>,
    pub arity: Option<u32>,
    pub range: TextRange,
    pub resolved: Option<ResolvedTarget>,
}
```

### 8.5 ReferenceId 规则

`ReferenceId` 必须包含语义角色：

```text
file_id
scope/source
kind
start_byte
end_byte
text
receiver role if any
arity if needed
```

绝对不能再让 `call` 与 `field_access` 因同 range/text 互相覆盖。

---

## 9. Call Graph v2

### 9.1 Callsite

```rust
pub struct Callsite {
    pub id: CallsiteId,
    pub file_id: FileId,
    pub caller: Option<SymbolId>,
    pub callee: Option<SymbolId>,
    pub callee_candidates: Vec<SymbolId>,
    pub receiver_node: Option<DataNodeId>,
    pub reference_id: Option<ReferenceId>,
    pub range: TextRange,
}
```

### 9.2 CallsiteArg

```rust
pub struct CallsiteArg {
    pub callsite_id: CallsiteId,
    pub index: u32,
    pub name: Option<String>,
    pub expr_id: HirExprId,
    pub data_node: DataNodeId,
    pub expr_text: String,
    pub range: TextRange,
}
```

### 9.3 Call edges

不要把 call edge 混在万能 `edges` 中。

推荐：

```sql
call_edges (
    callsite_id BLOB NOT NULL,
    caller BLOB,
    callee BLOB NOT NULL,
    confidence REAL,
    resolved_by TEXT,
    provenance TEXT
)
```

---

## 10. DataFlowGraph v2

### 10.1 DataNode

```rust
pub struct DataNode {
    pub id: DataNodeId,
    pub file_id: FileId,
    pub function_id: Option<SymbolId>,
    pub kind: DataNodeKind,
    pub binding_id: Option<BindingId>,
    pub expr_id: Option<HirExprId>,
    pub callsite_id: Option<CallsiteId>,
    pub name: Option<String>,
    pub access_path: Option<AccessPath>,
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
```

### 10.2 DataFlowEdge

```rust
pub struct DataFlowEdge {
    pub id: DataFlowEdgeId,
    pub source: DataNodeId,
    pub target: DataNodeId,
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
    Unknown,
}
```

### 10.3 示例

代码：

```ts
function handler(req) {
  const name = req.body.name;
  sink(name);
}
```

期望 dataflow：

```text
DataNode(req)
DataNode(req.body)
DataNode(req.body.name)
DataNode(name)
DataNode(sink.arg0)

req -> req.body
req.body -> req.body.name
req.body.name -> name
name -> sink.arg0
```

---

## 11. CFG v2

### 11.1 CfgNode

```rust
pub struct CfgNode {
    pub id: CfgNodeId,
    pub function_id: SymbolId,
    pub kind: CfgNodeKind,
    pub stmt_id: Option<HirStmtId>,
    pub range: TextRange,
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
```

### 11.2 CfgEdge

```rust
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
```

### 11.3 用途

- path-sensitive taint。
- sanitizer 是否支配 sink。
- return/throw 可达性。
- impact slicing。

---

## 12. Project Resolution v2

### 12.1 Resolver 分层

```text
ResolutionOrchestrator
  ├── LocalResolver
  ├── ImportResolver
  ├── ExportResolver
  ├── TypeResolver
  ├── MemberResolver
  ├── CallResolver
  └── BuiltinExternalResolver
```

### 12.2 LocalResolver

负责：

- lexical binding。
- scope chain。
- shadowing。
- same-file symbols。

### 12.3 ImportResolver / ExportResolver

负责：

- TS/JS import/export/re-export。
- TS path alias。
- Python package / relative import / `__init__.py`。
- Java package/static/wildcard。
- C/C++ include graph。
- Cangjie import。

### 12.4 MemberResolver

负责：

```text
obj.method
this.field
super.method
Class.staticMethod
namespace::symbol
```

需要：

- declared type。
- inferred type heuristic。
- class hierarchy。
- imports。
- namespace path。

### 12.5 CallResolver

负责：

- callsite callee candidates。
- overloaded functions。
- constructors。
- dynamic dispatch。
- function values。
- method calls。

### 12.6 Resolver 不创建 edges

v2 必须遵守：

```text
Resolver updates resolved facts.
GraphBuilder creates graph edges.
```

---

## 13. Store / Schema v2

### 13.1 Core tables

```sql
files
modules
symbols
scopes
imports
exports
references
```

### 13.2 Binding tables

```sql
bindings
binding_uses
```

### 13.3 Call graph tables

```sql
callsites
callsite_args
call_edges
```

### 13.4 Dataflow tables

```sql
data_nodes
dataflow_edges
function_summaries
```

### 13.5 CFG tables

```sql
cfg_nodes
cfg_edges
```

### 13.6 Analysis tables

```sql
taint_rules
taint_findings
taint_path_steps
impact_slices
```

### 13.7 Symbol graph edges

```sql
symbol_edges
```

用于：

- contains
- extends
- implements
- imports
- exports
- references
- overrides
- decorates

不要再让 `symbol_edges` 表示局部 dataflow。

---

## 14. ID 设计

新增 ID 类型：

```text
FileId
ModuleId
SymbolId
ScopeId
BindingId
BindingUseId
ReferenceId
HirItemId
HirStmtId
HirExprId
CallsiteId
DataNodeId
DataFlowEdgeId
CfgNodeId
CfgEdgeId
TaintFindingId
```

原则：

1. 每种 ID 只用于一个语义域。
2. 不用 `SymbolId` 表达虚拟 dataflow node。
3. ID 输入必须包含语义角色，避免 `INSERT OR REPLACE` 覆盖不同语义事实。
4. 文件内 HIR 临时 ID 与全局稳定 DB ID 分离。

---

## 15. Graph 分层

v2 不应有一个万能 GraphSnapshot。

建议：

```text
SymbolGraph
  - 全量 snapshot 可接受
  - 支持 search/context/contains/inheritance/imports

CallGraph
  - 可以全量或按需加载
  - 支持 callers/callees/impact

BindingGraph
  - 按文件/函数加载
  - 支持 def-use

DataFlowGraph
  - 按函数/切片加载
  - 支持 dataflow/taint

CFG
  - 按函数加载
  - 支持 path-sensitive analysis
```

原则：

- SymbolGraph 可全量。
- DataFlowGraph 和 CFG 不应默认全量加载大型项目。
- Analysis engine 应通过 query API 分块读取。

---

## 16. Productization：追平 CodeGraph

v2 重构时必须同时补齐产品化能力。

### 16.1 Index worker architecture

推荐：

```text
IndexOrchestrator
  ├── DiscoveryWorker
  ├── ParseWorkerPool
  ├── FrontendWorkerPool
  ├── StoreWriter
  └── ResolutionQueue
```

能力：

- parse timeout
- max file size
- worker isolation
- worker recycle
- grammar panic recovery
- retry
- generated/minified file skip
- structured index report
- file lock

### 16.2 Index report

写入：

```text
.atlas/index_report.json
```

示例：

```json
{
  "files_discovered": 1000,
  "files_indexed": 980,
  "files_skipped": 10,
  "files_failed": 10,
  "failures_by_category": {
    "parse_timeout": 2,
    "query_error": 3,
    "io_error": 1
  },
  "references_total": 50000,
  "references_resolved": 37000,
  "resolution_rate": 0.74,
  "duration_ms": 12345
}
```

### 16.3 Search query language

支持：

```text
kind:function lang:typescript path:src name:auth authenticate
```

后续扩展：

```text
edge:calls caller:foo callee:bar
taint:source path:api
symbol:UserService refs:true
```

### 16.4 Agent/MCP 体验

MCP tools 应支持：

- symbol search
- get references
- get callers/callees
- get dataflow slice
- get taint findings
- explain taint path
- get index diagnostics

---

## 17. Taint Analysis v2

### 17.1 依赖图

污点分析依赖：

```text
BindingGraph
  + DataFlowGraph
  + CallGraph
  + CFG
  + FunctionSummary
  + TaintRules
```

### 17.2 TaintRule

```rust
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
    Source,
    Sink,
    Sanitizer,
    Propagator,
}
```

规则文件示例：

```yaml
sources:
  - id: express.req.query
    language: typescript
    pattern: Request.query
    access_path: "*.query.*"

sinks:
  - id: node.child_process.exec
    language: typescript
    callee: child_process.exec
    argument: 0

sanitizers:
  - id: escape.html
    callee: escapeHtml
    return: true
```

### 17.3 FunctionSummary

```rust
pub struct FunctionSummary {
    pub function_id: SymbolId,
    pub input_flows: Vec<SummaryFlow>,
    pub sources: Vec<SummarySource>,
    pub sinks: Vec<SummarySink>,
    pub sanitizers: Vec<SummarySanitizer>,
    pub side_effects: Vec<SummaryEffect>,
}
```

示例：

```ts
function getName(req) {
  return req.query.name;
}
```

summary：

```text
param0.query.name -> return
```

### 17.4 Analysis flow

```text
1. Load source rules
2. Mark source DataNodes tainted
3. Intraprocedural propagation through dataflow edges
4. At callsite:
   arg -> callee param
   callee summary return -> call return
5. Check sink args
6. Check sanitizer edges
7. Emit finding path
```

### 17.5 Finding output

```rust
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

---

## 18. 全量重构实施策略

虽然是“全量干净重构”，但不建议一次性删除旧 pipeline。推荐并行建设 v2，再逐步切换默认入口。

### 18.1 阶段 A：v2 types + schema

任务：

- 新增 HIR 类型。
- 新增 Binding/DataFlow/CFG/Taint 类型。
- 新增 schema vNext。
- 新增 store writer/reader API skeleton。

不要求接入 CLI。

### 18.2 阶段 B：TS/Python v2 frontend

任务：

- 实现 TS/JS/ArkTS frontend lowering。
- 实现 Python frontend lowering。
- 生成 HIR。
- 建立 fixtures。

验收：

- definitions/imports/scopes/statements/expressions HIR 正确。

### 18.3 阶段 C：SemanticBinder v2

任务：

- ScopeBinder。
- SymbolBinder。
- LexicalBinder。
- ReferenceBinder。
- CallsiteBinder。
- DataFlowBuilder 初版。
- CfgBuilder 初版。

验收：

- TS/Python fixture 能产出 BoundFileFacts。
- def-use、callsite args、assignment flow 正确。

### 18.4 阶段 D：v2 Store

任务：

- `insert_bound_file_facts_v2()`。
- per-file delete cascade。
- v1/v2 表并存。

验收：

- v2 facts 能完整落库。
- delete/reindex 文件不会留下 stale facts。

### 18.5 阶段 E：v2 Resolver

任务：

- LocalResolver。
- ImportResolver 初版。
- CallResolver 初版。
- resolved facts 更新。
- GraphBuilder 创建 symbol/call edges。

验收：

- 跨文件 TS/Python call graph 正确。

### 18.6 阶段 F：Dataflow + Taint MVP

任务：

- intraprocedural dataflow。
- simple interprocedural summary。
- source/sink/sanitizer rules。
- finding path。

验收：

```ts
function getName(req) { return req.query.name; }
function handler(req) {
  const x = getName(req);
  sink(x);
}
```

能输出：

```text
req.query.name -> getName.return -> x -> sink.arg0
```

### 18.7 阶段 G：产品化切换

任务：

- `atlas index --pipeline v2`。
- v1/v2 output compare。
- index report。
- worker timeout/retry。
- query language。
- MCP tools。

### 18.8 阶段 H：v2 默认，v1 legacy 删除

条件：

- TS/Python/Java/C/C++/ArkTS/Cangjie 至少恢复 v1 主要能力。
- v2 tests 全绿。
- 大型 example 项目 index 通过。
- MCP/search/context 可用。

---

## 19. Agent 分工建议

### Agent A：v2 Core Types + Schema

负责：

- `hir/`
- `types` 新 ID。
- Binding/DataFlow/CFG/Taint types。
- schema vNext。

禁止：

- 写语言 frontend 细节。
- 写 taint engine。

### Agent B：Frontend TS/Python

负责：

- TS/JS/ArkTS HIR lowering。
- Python HIR lowering。
- fixtures。

参考：

- 当前 `src/extraction/languages/typescript.rs`
- 当前 `src/extraction/languages/python.rs`
- `codegraph/src/extraction/tree-sitter.ts`
- `codegraph/src/extraction/languages/typescript.ts`

### Agent C：SemanticBinder v2

负责：

- ScopeBinder。
- SymbolBinder。
- LexicalBinder。
- ReferenceBinder。
- CallsiteBinder。

### Agent D：DataFlow + CFG

负责：

- DataFlowBuilder。
- CfgBuilder。
- DataNode/DataFlowEdge 落库。

### Agent E：Resolution v2

负责：

- LocalResolver。
- ImportResolver。
- MemberResolver。
- CallResolver。
- GraphBuilder。

### Agent F：Productization

负责：

- worker pool。
- timeout。
- retry。
- max file size。
- index report。
- file lock。
- query parser。

参考：

- `codegraph/src/extraction/index.ts`
- `codegraph/src/extraction/parse-worker.ts`
- `codegraph/src/search/query-parser.ts`

### Agent G：Taint MVP

负责：

- Taint rules。
- Taint engine。
- Function summaries。
- Finding path。
- MCP/CLI 输出。

依赖：

- Agent D 的 DataFlowGraph。
- Agent E 的 CallResolver。

---

## 20. v2 验收标准

### 20.1 基础正确性

- `cargo test --all-features` 全绿。
- TS/Python v2 fixture 全绿。
- 不存在 ghost symbol source。
- 不存在 `ReferenceId` 语义冲突覆盖。
- callsite args 正确落库。

### 20.2 CodeGraph parity

- 支持 max file size。
- 支持 parse timeout。
- 支持 index report。
- 支持 query parser。
- 大项目坏文件不拖垮整体 index。

### 20.3 Dataflow MVP

必须通过：

```ts
function f(req) {
  const name = req.body.name;
  return name;
}
```

期望：

```text
req -> req.body -> req.body.name -> name -> return
```

### 20.4 Cross-function dataflow MVP

必须通过：

```ts
function getName(req) {
  return req.query.name;
}
function handler(req) {
  const x = getName(req);
  sink(x);
}
```

期望：

```text
req.query.name -> getName.return -> x -> sink.arg0
```

### 20.5 Taint MVP

- source/sink/sanitizer rule 可配置。
- finding 包含完整路径。
- MCP 能解释 taint path。

---

## 21. 不应做的事

1. 不要继续把 dataflow target 伪装成 `SymbolId`。
2. 不要让 adapter 继续生成最终 source owner。
3. 不要让 resolver 直接创建 graph edges。
4. 不要把所有局部变量都塞进 `SymbolDef`。
5. 不要用一个万能 `edges` 表承载 symbol/call/dataflow/cfg。
6. 不要为了短期方便绕过 diagnostics。
7. 不要在 v2 中先支持所有语言再验证核心抽象；应先 TS/Python 打透。

---

## 22. 最终愿景

Atlas v2 完成后不应该只是 CodeGraph 的 Rust 版本。

目标是：

```text
CodeGraph parity in UX/productization
  + Rust-native typed semantic platform
  + compiler-style HIR/binder
  + layered graph facts
  + cross-function dataflow
  + taint analysis
  + Agent-explainable reasoning
```

一句话总结：

> v1 是符号索引器。  
> v2 应该是多语言语义编译器和程序分析平台。  
> 搜索靠 SymbolGraph，调用靠 CallGraph，污点分析靠 BindingGraph + DataFlowGraph + CFG + FunctionSummary。  
> 这些层必须干净分离，不能继续塞进一个 RawEdge 模型里。
