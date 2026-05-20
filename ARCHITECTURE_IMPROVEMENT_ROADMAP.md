# Atlas 架构改进路线图：追平 CodeGraph 产品化能力并升级到高阶程序分析

> 面向后续开发 Agent 的实施指导文档。  
> 当前目标不是只修某个语言的 `.scm`，而是把 Atlas 从“符号索引 / 简单调用图工具”升级为“可产品化使用、并可承载污点分析和跨函数数据流的语义分析平台”。

---

## 0. 文档目的

本文用于指导多个 Agent 并行推进 Atlas 的架构演进，明确：

1. 当前 Atlas 架构存在的问题及其前因后果。
2. Atlas 与本仓库内 `codegraph/` 的核心差异。
3. Atlas 需要补齐哪些易用性、鲁棒性、产品化能力以追上 CodeGraph。
4. Atlas 若要支持污点分析、跨函数数据流、影响分析等高阶关联操作，需要新增哪些基础模型和分析层。
5. 推荐的改造阶段、模块边界、验收标准和 Agent 分工方式。

本文中的路径均相对于项目根目录。

---

## 1. 当前 Atlas 的定位与核心需求

### 1.1 当前 Atlas 已具备的基础能力

Atlas 当前是 Rust 原生语义索引器，主要架构为：

```text
sync/discovery
  ↓
extraction
  - LanguageAdapter
  - queries/<lang>/*.scm
  - normalize_definition/reference/import/scope/dataflow
  - build_scope_tree
  - SymbolRegistry
  ↓
types IR
  - SymbolDef
  - ReferenceUse
  - ImportDef
  - ScopeDef
  - RawEdge
  - Callsite
  ↓
db/store + schema
  - files
  - symbols
  - scopes
  - references_v2
  - imports
  - edges
  - callsites
  ↓
resolution
  - BuiltinFilter
  - scoped lookup
  - same-file lookup
  - import resolver
  - project-wide fuzzy search
  ↓
graph/search/context/mcp
```

核心优势：

- Rust 原生实现，适合后续高性能分析引擎。
- 强类型 IR：`SymbolDef`、`ReferenceUse`、`ImportDef`、`ScopeDef`、`RawEdge`、`Callsite`。
- typed deterministic IDs：`FileId`、`SymbolId`、`ReferenceId`、`EdgeId`、`CallsiteId`。
- `references_v2` 持久化保存引用，resolved 后仍保留引用记录，适合审计和解释。
- `SymbolRegistry` 已经初步解决 definitions 与 references/dataflow 之间 source ownership 不一致的问题。
- CLI index 已经变为“并行提取 + 顺序写 SQLite + 批量 resolve”。

### 1.2 Atlas 接下来的核心需求

Atlas 未来需要同时满足两类需求：

#### A. 产品化语义索引工具

面向开发者和 Agent：

- 快速可靠地 index 大型仓库。
- 支持稳定增量 sync。
- 支持精确 symbol/search/context/MCP 查询。
- 提供清晰错误摘要、诊断、doctor、可观测指标。
- 易安装、易集成、易被 Agent 使用。
- 在易用性和工程鲁棒性上追上或超过 CodeGraph。

#### B. 高阶程序分析平台

面向安全审计、影响分析、跨函数推理：

- 支持局部变量、参数、字段、表达式级事实。
- 支持 def-use、dataflow、callsite 参数、返回值传播。
- 支持 interprocedural function summary。
- 支持 source/sink/sanitizer 规则系统。
- 支持 taint path 输出和解释。
- 支持跨文件、跨模块、跨语言项目中的语义关联。

---

## 2. 当前问题的前因后果

### 2.1 原始根因：source ownership 分裂

历史问题：

```text
definitions.scm -> normalize_definition() -> SymbolId A
references/dataflow -> find_enclosing_*() -> SymbolId B
```

当 A 与 B 不一致时，会产生 ghost source symbol。由于 `edges.source` 和 `callsites.caller` 有 FK 约束，单个错误 edge/callsite 会导致整文件事实写入失败。

典型场景：

```ts
export const af = (x: number) => x + 1;
```

- definitions 把 `af` 当 `Variable`。
- dataflow / reference ownership 可能把 `af` 当 `Function`。
- 两边 SymbolId 不同。
- edge/callsite 指向不存在的 source。

当前修复：

- `src/extraction/symbol_registry.rs` 从实际提取出的 `symbols + scopes` 构建 registry。
- `extract_file()` 在 `build_scope_tree()` 后统一重写 reference/edge source。
- `store.insert_file_facts()` 加 defensive FK guard。

这是正确方向，但还不是最终架构。

### 2.2 当前仍然存在的后续问题

#### 问题 1：Adapter 仍在自己生成 source symbol

各语言 adapter 仍然有：

- `find_enclosing_function_id`
- `find_enclosing_function_id_py`
- `find_enclosing_method_id`
- `find_enclosing_function_id_c`
- `find_enclosing_function_id_cpp`
- `find_enclosing_function_id_cj`

虽然 `SymbolRegistry` 会后置覆盖，但架构上仍是双路径。

后果：

- 新语言容易继续复制错误模式。
- `ReferenceId` 初始生成与后续重写不一致。
- 维护成本高。

改进方向：

- Adapter 只负责把 AST/query capture 降级成 raw fact。
- `source_symbol`、`scope_id` 统一由 `SemanticBinder` / `SymbolRegistry` 填充。

#### 问题 2：`ReferenceUse.scope_id` 基本没有被填充

当前 resolver 第一阶段依赖：

```rust
if let Some(scope_id) = reference.scope_id {
    ctx.lookup_scoped(scope_id, &reference.name)
}
```

但各语言 normalizer 大多写：

```rust
scope_id: None
```

后果：

- scope-local resolution 基本失效。
- 同名局部变量、参数、字段、函数难以精确区分。
- 污点分析无法做可靠 def-use。

改进方向：

- `SymbolRegistry` 扩展为 `SemanticBinder`。
- 为每个 reference 填充 innermost scope。
- 引入 binding 表，建立 `identifier use -> binding def`。

#### 问题 3：`ReferenceId` 缺少 `ReferenceKind`

当前 `ReferenceId::generate()` 只基于：

```text
file_id + source_symbol + byte_range + text
```

没有包含：

- `ReferenceKind`
- receiver
- arity
- capture role

典型冲突：

```ts
obj.method()
```

同一个 property_identifier 可能同时被捕获为：

- `reference.call`
- `reference.field`

二者 byte range 和 text 相同，导致 `reference_id` 相同，`INSERT OR REPLACE` 后互相覆盖。

后果：

- callsite 派生不稳定。
- call graph 不稳定。
- field read 与 method call 混淆。
- 污点传播无法区分 read/call/write。

改进方向：

- `ReferenceId::generate()` 必须加入 `ReferenceKind`。
- 推荐额外加入 receiver/arity/capture role，确保不同语义角色不碰撞。

#### 问题 4：dataflow 当前不是数据流

当前 `RawEdge` 是：

```rust
source: SymbolId
target: SymbolId
kind: EdgeKind
```

dataflow normalizer 生成的是：

```text
function_symbol -> virtual_dataflow_symbol
```

例如：

```text
function add -> dataflow target "a" kind parameter
function add -> dataflow target "result" kind assigns
```

这些 target 不是实际 symbol。`edges.target` 没有 FK，`GraphSnapshot` 也会跳过 target 不在 symbols 的边。

后果：

- SQLite 中有 dataflow 边，但 GraphSnapshot 中不可用。
- 无法表达 `y -> x`、`arg -> param`、`return -> assignment`。
- 无法支撑污点分析。

改进方向：

- dataflow 必须从 `RawEdge<SymbolId, SymbolId>` 中拆出来。
- 新增 `DataNode`、`DataFlowEdge`、`CallsiteArg`、`FunctionSummary`。

#### 问题 5：import/module resolution 不够

当前 `ImportDef` 有字段，但各语言 normalizer 不稳定：

- TS/JS alias 丢失 imported/local 语义。
- Python alias 不完整。
- Java static/wildcard import 未正确建模。
- C/C++ include 未规范化系统 include。
- TS path alias / barrel re-export 缺失。

后果：

- 跨文件 reference 解析弱。
- call graph 断裂。
- taint source/sink 跨模块无法追踪。

改进方向：

- 引入 import/export/re-export resolver。
- 支持 TS path alias、Python package、Java package、C/C++ include graph。

#### 问题 6：incremental sync 缺少 resolved fact invalidation

当前 resolver 主要处理：

```sql
WHERE resolved_symbol_id IS NULL
```

文件变更后，已 resolved 的 reference 不一定重新解析。

后果：

- 删除/改名后的 symbol 可能仍被旧 reference 指向。
- edge.target 没有 FK，可能保留悬空 target。
- 污点分析可能基于 stale graph 得出错误结论。

改进方向：

- 文件删除/变更后反向查找受影响 references。
- 清空 stale `resolved_symbol_id`。
- 删除相关 resolved edges。
- 重跑受影响 references 的 resolution。

#### 问题 7：parser/extraction 鲁棒性不足

Atlas 当前 Rayon 并行提取，但缺少：

- per-file parse timeout
- worker 隔离
- grammar crash recovery
- worker recycle
- max file size
- OOM retry
- comment stripping retry
- file lock

CodeGraph 已经具备这些能力。

后果：

- 大项目中单个问题文件可能拖垮 index。
- grammar 崩溃可能影响整个进程。
- 用户体验不如 CodeGraph。

改进方向：

- 建立 parse worker 池或进程隔离。
- 增加超时、重试、跳过、诊断摘要。
- 增加 `.atlasignore`、max file size、file lock、error report。

---

## 3. Atlas 与 CodeGraph 的关键差异

### 3.1 总体对比

| 维度 | Atlas 当前 | CodeGraph 当前 | Atlas 改进方向 |
|---|---|---|---|
| 实现语言 | Rust | TypeScript/Node | 保持 Rust，强化 native performance |
| Extraction | `.scm` query + normalize | AST walker + language config/hooks | 引入 normalized frontend/binder，降低 `.scm` 与 adapter 分裂 |
| Source ownership | `SymbolRegistry` 后处理 | `nodeStack` 单遍维护 | `SemanticBinder` 统一填充 source/scope/binding |
| IR | 强类型多表 IR | 简化 Node/Edge/UnresolvedRef | 保持强类型，同时扩展到 Binding/DataFlow/CFG |
| Reference 保存 | 持久保存 reference + resolved metadata | resolved 后转 edge 并删除 unresolved refs | 保留 Atlas 审计优势 |
| Graph | 全量 GraphSnapshot | SQLite on-demand traversal + cache | SymbolGraph 可 snapshot，DataFlowGraph 分块加载 |
| 鲁棒性 | Rayon 并行，无 worker 隔离 | worker timeout/recycle/retry | 补 worker 池、timeout、重试、size cap |
| 搜索 | FTS + fuzzy + CLI flags | field query parser | 增加 `kind: lang: path: name:` 查询语法 |
| Framework | stub 为主 | 多 framework resolver/extractor | 将 framework resolver 接入主流程 |
| 高阶分析 | 尚未具备 | 也不是完整 taint 平台 | Atlas 要向 CPG/DFG/CFG 演进 |

### 3.2 Atlas 相对 CodeGraph 的优势

1. **强类型 IR 更适合分析平台**  
   Atlas 的 `SymbolDef`、`ReferenceUse`、`ScopeDef`、`ImportDef` 等比 CodeGraph 的通用 Node/Edge 更适合做严肃分析。

2. **Reference 持久化更适合审计**  
   resolved 后仍保留 reference，有利于解释 resolution provenance、confidence、strategy。

3. **typed deterministic ID 更安全**  
   BLAKE3 typed ID 比字符串 ID 更适合跨表、跨阶段一致性。

4. **Rust 更适合构建重分析引擎**  
   后续 CFG/DFG/taint worklist、SCC summary、large graph traversal 均适合 Rust。

### 3.3 CodeGraph 相对 Atlas 的优势

1. **产品化索引能力更成熟**  
   parse worker、timeout、worker recycle、OOM retry、comment stripping retry、max file size 都值得 Atlas 学习。

2. **语言和框架覆盖更广**  
   CodeGraph 已支持更多语言和 Vue/Svelte/Liquid/Express/Ruby/Rust 等框架。

3. **AST walker 的 ownership 更自然**  
   `nodeStack` 让 contains/calls/source ownership 在单遍 traversal 中天然一致。

4. **搜索语法更友好**  
   `kind:function lang:typescript path:src name:auth` 这种查询对 Agent 很友好。

5. **import/path alias/re-export 更成熟**  
   对 JS/TS 项目尤其重要。

---

## 4. 目标架构

Atlas 应从当前 pipeline：

```text
Extraction -> Store -> Resolution -> Graph
```

升级为：

```text
Parse
  ↓
Language Frontend
  - query / AST lowering
  - normalized constructs
  ↓
Semantic Binding
  - scopes
  - bindings
  - references
  - imports/exports
  - source ownership
  ↓
Graph Construction
  - SymbolGraph
  - BindingGraph
  - CallGraph
  - DataFlowGraph
  - CFG
  ↓
Resolution
  - local binding resolution
  - import/module resolution
  - type/member resolution
  - call target resolution
  ↓
Analysis Engines
  - search/context/MCP
  - impact analysis
  - taint analysis
  - cross-function dataflow
```

### 4.1 模块建议

新增或重构模块：

```text
src/extraction/
  frontend/              # 语言前端：query/AST capture -> normalized raw facts
  semantic_binder.rs     # source_symbol/scope_id/binding resolution
  symbol_registry.rs     # 可并入 semantic_binder 或保留为子组件

src/analysis/
  mod.rs
  dataflow/
    mod.rs
    ir.rs
    intraprocedural.rs
    interprocedural.rs
    summaries.rs
  taint/
    mod.rs
    rules.rs
    engine.rs
    findings.rs
  cfg/
    mod.rs
    builder.rs
    ir.rs

src/types/
  bindings.rs
  dataflow.rs
  cfg.rs
  taint.rs

src/db/
  schema.rs              # 增加新表和 migration
  store.rs               # 增加新 fact 写入/查询 API
```

---

## 5. 必须新增的核心 IR

### 5.1 Binding 模型

用途：支持局部变量、参数、import alias、catch var、field binding。

建议类型：

```rust
pub struct BindingDef {
    pub id: BindingId,
    pub file_id: FileId,
    pub function_id: Option<SymbolId>,
    pub scope_id: ScopeId,
    pub kind: BindingKind,
    pub name: String,
    pub range: TextRange,
    pub symbol_id: Option<SymbolId>,
}

pub enum BindingKind {
    Parameter,
    Local,
    Field,
    ImportAlias,
    CatchVariable,
    Global,
}

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

验收标准：

- `function f(a) { let b = a; return b; }` 能解析：
  - parameter binding `a`
  - local binding `b`
  - use `a -> parameter a`
  - use `b -> local b`

### 5.2 DataNode / DataFlowEdge

用途：表达表达式级数据流，而不是函数到虚拟 symbol 的弱边。

建议类型：

```rust
pub struct DataNode {
    pub id: DataNodeId,
    pub file_id: FileId,
    pub function_id: SymbolId,
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
    Unknown,
}

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
}
```

示例：

```ts
const name = req.body.name;
sink(name);
```

应表达：

```text
req -> req.body
req.body -> req.body.name
req.body.name -> name
name -> sink.arg0
```

### 5.3 CallsiteArg

当前 `Callsite.args` 基本为空，必须补齐。

建议：

```rust
pub struct CallsiteArg {
    pub callsite_id: CallsiteId,
    pub index: u32,
    pub name: Option<String>,
    pub expr_text: String,
    pub data_node: DataNodeId,
    pub range: TextRange,
}
```

并在 resolution 后回填：

```text
callsite.callee
callsite.arg[i] -> callee.param[i]
callee.return -> callsite.return
```

### 5.4 CFG

最低限度的函数内 CFG：

```rust
pub struct CfgNode {
    pub id: CfgNodeId,
    pub function_id: SymbolId,
    pub kind: CfgNodeKind,
    pub range: TextRange,
}

pub struct CfgEdge {
    pub source: CfgNodeId,
    pub target: CfgNodeId,
    pub kind: CfgEdgeKind,
}
```

用途：

- path-sensitive taint。
- sanitizer 是否支配 sink。
- return/throw/branch/loop 的可达性分析。

### 5.5 FunctionSummary

跨函数分析不能无限 inline，必须做 summary。

建议：

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

### 5.6 TaintRule / TaintFinding

规则系统：

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

finding：

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

## 6. 数据库改造建议

保留现有表：

```text
files
symbols
scopes
references_v2
imports
edges
callsites
```

新增表：

```text
bindings
binding_uses
data_nodes
dataflow_edges
callsite_args
cfg_nodes
cfg_edges
function_summaries
taint_rules
taint_findings
taint_path_steps
```

关键原则：

1. `edges` 继续表示 symbol-level graph。  
   不再用它表达局部变量数据流。

2. `dataflow_edges` 使用 `DataNodeId -> DataNodeId`。  
   不再使用虚拟 `SymbolId`。

3. `references_v2.resolved_symbol_id` 应考虑 FK：  
   `REFERENCES symbols(symbol_id) ON DELETE SET NULL`。

4. `edges.target` 如果只用于 symbol graph，应加 FK。  
   如果需要 unknown/external target，应显式建 `ExternalSymbol` 或 nullable target，不要混用虚拟 ID。

5. 所有新增 facts 都必须支持 per-file delete cascade。  
   增量 sync 必须可安全删除某文件 facts 并重建。

---

## 7. 分阶段实施计划

### P0：正确性和稳定性基线

目标：先让当前 symbol/reference/call graph 稳定。

任务：

1. 修复 Cangjie 当前 all-features 测试失败。
   - `src/extraction/queries/cangjie/references.scm` 中 `typeAnnotation` 与当前 grammar 不兼容。
   - `cj_scope_kind("scope.interface")` 应返回 `ScopeKind::Interface`。

2. 修改 `ReferenceId`。
   - 输入加入 `ReferenceKind`。
   - 推荐同时加入 receiver/arity/capture role。

3. `SymbolRegistry` 扩展填充 `ReferenceUse.scope_id`。

4. 降低 adapter source ownership 职责。
   - adapter 不再生成最终 `source_symbol`。
   - source/scope ownership 统一在 binder 阶段完成。

5. 修复 call/member duplicate。
   - 同一 member call 不应被不稳定覆盖。
   - 若保留 call + field 两条 reference，ID 必须不同。

6. 修复语言 metadata。
   - TSX/JSX extension detection/globs。
   - ArkTS `.sts`。
   - Java constructor kind。
   - C/C++ include normalize。

7. 建立 per-language golden tests。
   - 每个语言至少覆盖 symbols/references/imports/scopes/callsites。
   - 测试 fixture 存放建议：`tests/fixtures/<lang>/`。

验收：

```bash
cargo test --all-features
```

必须全绿。

### P1：追平 CodeGraph 产品化索引能力

目标：大项目可用，失败可控，Agent 体验友好。

任务：

1. Parse worker 隔离。
   - Rust 可使用 worker thread pool 或 child process。
   - 每个文件 parse 有 timeout。
   - grammar panic/OOM 不拖垮整个 index。

2. 增加 max file size。
   - 配置项：`.atlas/config.toml` 或 CLI 参数。
   - 跳过超大/minified/generated 文件。

3. worker recycle / retry。
   - 处理 parser heap/memory 膨胀。
   - 失败文件可 retry 一次。
   - 可选 comment-only stripping fallback。

4. 错误摘要产品化。
   - 按 error category 聚合。
   - 输出 top N 文件 + first error。
   - 保存到 `.atlas/index_report.json`。

5. 文件锁。
   - 避免多个 `atlas index/sync` 同时写 DB。

6. 搜索 query parser。
   - 支持：`kind:`, `lang:`, `path:`, `name:`。
   - 对齐 CodeGraph 的 agent-friendly 查询体验。

7. installer / MCP 使用文档。
   - 提供 Codex/Claude/Cursor/OpenCode 指令模板。

验收：

- 大型 example 项目 index 不因单文件失败中断。
- CLI 输出清晰告诉用户 skipped/failed/resolved 情况。
- MCP/search/context 对 Agent 足够可用。

### P2：模块解析与 call graph 精度

目标：跨文件、跨模块调用关系稳定。

任务：

1. Import alias 语义修复。
   - TS/JS/Python/Java/C++ 各自 golden tests。

2. Re-export / barrel 支持。
   - TS `export { x as y } from './m'`。
   - TS `export * from './m'`。

3. TS path alias。
   - 读取 `tsconfig.json` / `jsconfig.json` paths/baseUrl。

4. Python package/module resolution。
   - relative import。
   - `__init__.py` re-export。

5. Java package declaration + static/wildcard imports。

6. C/C++ include graph。
   - system include vs local include。
   - header/source pairing。
   - declaration/definition 合并策略。

7. callsite.callee 回填。

8. resolved fact invalidation。
   - 修改/删除文件后清空受影响 references。
   - 删除 stale edges。

验收：

- 跨文件调用图稳定。
- rename/delete 后 sync 不保留悬空 resolved target。
- GraphSnapshot 中 symbol-level edges 不指向不存在 symbol。

### P3：BindingGraph 与 DataFlowGraph

目标：为污点分析打地基。

任务：

1. 新增 `BindingDef` / `BindingUse` IR 与 DB 表。
2. `SemanticBinder` 建立 lexical def-use。
3. 新增 `DataNode` / `DataFlowEdge` IR 与 DB 表。
4. 提取函数内：
   - parameters
   - locals
   - assignment lhs/rhs
   - returns
   - field read/write
   - call args
   - call return
5. `Callsite.args` 正式落库，不再为空。
6. 将旧 dataflow `RawEdge` 标记为 deprecated 或迁移。

验收示例：

```ts
function f(req) {
  const name = req.body.name;
  return sanitize(name);
}
```

必须能得到：

```text
req -> req.body -> req.body.name -> name -> sanitize.arg0
sanitize.return -> f.return
```

### P4：CFG 与跨函数 dataflow

目标：支持 interprocedural dataflow。

任务：

1. 函数内 CFG。
2. callsite arg-to-param。
3. callee return-to-call return。
4. function summary。
5. SCC/worklist 分析。
6. recursion / unknown call fallback。

验收：

```ts
function getName(req) { return req.query.name; }
function handler(req) {
  const x = getName(req);
  sink(x);
}
```

必须能追踪：

```text
req.query.name -> getName.return -> x -> sink.arg0
```

### P5：污点分析 MVP

目标：实现可解释的 source-to-sink 分析。

任务：

1. `taint_rules` schema。
2. YAML/TOML/JSON rule loader。
3. source/sink/sanitizer matching。
4. intraprocedural taint engine。
5. interprocedural taint engine。
6. finding path 存储和 CLI/MCP 输出。
7. 默认规则包：
   - JS/TS Express/Node：`req.query`, `req.body`, `child_process.exec`, SQL query。
   - Python Flask/FastAPI：`request.args`, `request.json`, SQL/command sinks。
   - Java Servlet/Spring：request params, SQL/Runtime.exec sinks。

验收：

- 能输出 source、sink、完整路径、跨函数步骤、sanitizer 是否存在。
- 结果包含 confidence 和 rule id。
- MCP 工具可查询某个 finding 的路径和相关源码片段。

---

## 8. Agent 开发分工建议

### Agent A：Correctness Foundation

负责：

- `ReferenceId` 改造。
- `ReferenceUse.scope_id` 填充。
- Cangjie test 修复。
- call/member collision 修复。
- per-language golden tests 框架。

主要文件：

```text
src/types/ids.rs
src/types/structs.rs
src/extraction/extract.rs
src/extraction/symbol_registry.rs
src/extraction/queries/*/*.scm
src/extraction/languages/*.rs
tests/
```

不得做：

- 大规模 DB schema 扩展。
- taint engine。

### Agent B：Productization / CodeGraph Parity

负责：

- parse timeout。
- worker isolation / worker pool。
- max file size。
- index report。
- file lock。
- search query parser。
- installer / MCP instructions。

主要文件：

```text
src/cli/commands/index.rs
src/sync/discovery.rs
src/sync/mod.rs
src/search/
src/mcp/
docs/
```

参考 CodeGraph：

```text
codegraph/src/extraction/index.ts
codegraph/src/extraction/parse-worker.ts
codegraph/src/search/query-parser.ts
codegraph/src/installer/
```

### Agent C：Import / Resolution Precision

负责：

- TS path alias。
- re-export/barrel。
- Python package import。
- Java package/static/wildcard import。
- C/C++ include graph。
- resolved invalidation。

主要文件：

```text
src/resolution/import_resolver.rs
src/resolution/context.rs
src/resolution/mod.rs
src/db/store.rs
src/db/schema.rs
src/extraction/languages/*.rs
```

参考 CodeGraph：

```text
codegraph/src/resolution/import-resolver.ts
codegraph/src/resolution/path-aliases.ts
codegraph/src/resolution/index.ts
```

### Agent D：DataFlow Foundation

负责：

- `BindingDef` / `BindingUse`。
- `DataNode` / `DataFlowEdge`。
- callsite args。
- intraprocedural dataflow extraction。

主要文件：

```text
src/types/
src/extraction/
src/analysis/dataflow/
src/db/schema.rs
src/db/store.rs
```

不得复用：

```text
RawEdge target = virtual SymbolId
```

作为新的 dataflow 模型。

### Agent E：Taint Analysis MVP

负责：

- taint rule loader。
- source/sink/sanitizer matching。
- intraprocedural taint。
- interprocedural summary propagation。
- finding path 输出。

依赖：

- Agent D 的 DataFlowGraph。
- Agent C 的 call/import resolution。

主要文件：

```text
src/analysis/taint/
src/analysis/dataflow/summaries.rs
src/mcp/tools.rs
src/cli/commands/
```

---

## 9. 开发约束与验收规则

### 9.1 每个架构改造必须有 tests

至少包含：

- unit tests
- per-language fixture tests
- integration tests

新增语言 query 时必须验证：

```text
query compile
expected symbols
expected references
expected imports
expected scopes
expected callsites
```

### 9.2 不允许静默吞掉核心错误

可以 best-effort extraction，但必须记录 diagnostics：

- parse errors
- query compile errors
- normalization failures
- dropped ghost facts
- skipped large files
- timeout files

### 9.3 不允许用虚拟 SymbolId 继续扩展 dataflow

旧模型可以保留兼容，但新分析必须使用：

```text
DataNodeId -> DataNodeId
```

### 9.4 Graph 分层

- SymbolGraph：适合全量 snapshot。
- DataFlowGraph：按函数/文件/切片加载。
- CFG：按函数加载。
- TaintGraph：按 finding/path 查询。

### 9.5 ID 生成必须包含语义角色

例如 ReferenceId 必须区分：

- call
- field access
- type reference
- instantiation
- read
- write

否则后续 facts 会被 `INSERT OR REPLACE` 覆盖。

---

## 10. 近期建议的第一批 PR

建议按以下顺序开 PR，避免长期大分支：

1. **PR-1：Cangjie + ReferenceId correctness**
   - 修 Cangjie query failure。
   - `ReferenceId` 加 kind。
   - 更新所有调用点和 tests。

2. **PR-2：SemanticBinder scope assignment**
   - `SymbolRegistry` 扩展 `scope_for_range`。
   - 给 references 填 `scope_id`。
   - 删除/弱化 adapter source owner 生成。

3. **PR-3：call/member collision + callsite args skeleton**
   - 解决 member call 同时 field/call 的覆盖。
   - `Callsite.args` 至少捕获 text/index/range。

4. **PR-4：CodeGraph parity indexing robustness**
   - maxFileSize。
   - parse timeout。
   - index report。
   - file lock。

5. **PR-5：DataFlowGraph schema + TS/Python MVP**
   - DataNode/DataFlowEdge 表。
   - TS/Python assignment/return/call arg dataflow。

6. **PR-6：Taint MVP for TS/Python**
   - source/sink/sanitizer rules。
   - intraprocedural + simple cross-function summary。

---

## 11. 最终目标判断

Atlas 不应只是“Rust 版 CodeGraph”。

短期：

- 在易用性、鲁棒性、搜索体验、Agent 集成上追平 CodeGraph。

中期：

- 通过 BindingGraph、DataFlowGraph、CFG 和 FunctionSummary 超越 CodeGraph 的符号图能力。

长期：

- 成为可本地运行的语义程序分析平台，支持：
  - 污点分析
  - 跨函数数据流
  - 影响分析
  - 安全审计
  - 代码变更风险分析
  - Agent 可解释上下文构建

核心原则：

> Symbol graph 是搜索和上下文的基础，但不是污点分析的基础。  
> 污点分析必须建立在 binding、expression、callsite、dataflow、CFG 和 summary 之上。  
> Atlas 的下一阶段关键任务，就是把这些层补齐，并保持 Rust 强类型和 reference 可审计的优势。
