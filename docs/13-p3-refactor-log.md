# P3: BindingGraph + DataFlowGraph — 实施日志

> **基于** `ARCHITECTURE_IMPROVEMENT_ROADMAP.md` P3 和 `10-p1-p5-architecture-design.md`
>
> **实施日期**: 2026-05-20  
> **前置依赖**: P0 (架构重构), P1 (产品化), P2 (模块解析+调用图)

---

## 1. 架构与约束检查

在 P3 实施前，确认所有全局约束已满足：

| 约束 | 状态 |
|------|------|
| Adapter 不填 source_symbol/scope_id | ✅ P0 done |
| SemanticBinder 是 source/scope/binding 唯一权威 | ✅ P0 done |
| ReferenceId 包含 kind | ✅ P0 done |
| References 永不删除 | ✅ invariant |
| 新 dataflow 用 DataNodeId→DataNodeId，NOT SymbolId | ✅ P3 核心约束 |
| 每阶段有 golden tests | ✅ 本阶段完成 |
| 不静默吞错误 | ✅ 所有错误通过 IndexReport 暴露 |

---

## 2. P3 目标

P3 的核心目标是将 dataflow 从符号级 (`symbol_edges` 中的 RawEdge) 提升到专用数据流层：

1. **独立的类型系统** — BindingDef/BindingUse, DataNode/DataFlowEdge, CallsiteArg（不污染 SymbolId 命名空间）
2. **DB schema v5** — 5 张新表 + `edges` → `symbol_edges` 重命名
3. **LexicalBinder** — 从 AST 提取词法绑定定义（参数、局部变量、import 别名等）
4. **DataFlowBuilder** — 从 AST 提取数据流节点和边（赋值、返回、调用参数等）
5. **松耦合的 DB 存储** — 所有外键 `ON DELETE SET NULL`，提取阶段不要求 FK 指向有效 ID

### 3.1 不变式

```
DataFlowEdge.source ──► DataNodeId  (never SymbolId)
DataFlowEdge.target ──► DataNodeId  (never SymbolId)

BindingDef  ──► function_id (Option)
DataNode ──► function_id / binding_id / callsite_id (all Option)
```

---

## 4. 实施内容

### P3-1: 新增 ID 类型 (`src/types/ids.rs`, +257 lines)

使用 `define_id!` 宏新增 4 个 blake3 ID 类型：

| ID 类型 | 哈希输入 | 说明 |
|---------|---------|------|
| `BindingId` | file_id + scope_id + kind + name + start_byte | 词法绑定标识 |
| `BindingUseId` | file_id + binding_id? + reference_id? + name + start_byte | 绑定使用点标识 |
| `DataNodeId` | file_id + function_id? + kind + name? + access_path? + start_byte | 数据流节点标识 |
| `DataFlowEdgeId` | source_node + target_node + kind | 数据流边标识 |

所有 ID 类型遵循 P0 约束：deterministic blake3，无 UUID/自增。

新增 9 个单元测试验证 ID 确定性。

### P3-2: 新增枚举类型 (`src/types/enums.rs`, +190 lines)

| 枚举 | 值数量 | 说明 |
|------|--------|------|
| `BindingKind` | 7 | Parameter, Local, Field, ImportAlias, CatchVariable, LambdaParameter, Global |
| `DataNodeKind` | 11 | Parameter, Local, Field, Return, Literal, Expr, CallArg, CallReturn, Receiver, Global, Unknown |
| `DataFlowKind` | 10 | Assign, Read, Write, FieldLoad, FieldStore, ArgToParam, ReturnToCall, ReceiverToThis, Phi, Sanitized |

所有枚举实现 `as_str()` + `FromStr`（支持 serde round-trip）。

### P3-3: 新增结构体 (`src/types/bindings.rs` + `src/types/dataflow.rs`)

**bindings.rs** (~140 lines):
- `BindingDef` — 词法绑定定义（参数、局部变量、import 别名等）
  - 字段：id, file_id, function_id, scope_id, kind, name, symbol_id, range
- `BindingUse` — 绑定使用点（引用位置）
  - 字段：id, file_id, scope_id, binding_id, reference_id, name, range
- 2 个 serde round-trip 测试

**dataflow.rs** (~270 lines):
- `DataNode` — 数据流节点（参数、局部变量、字段、返回值等）
  - 字段：id, file_id, function_id, kind, binding_id, callsite_id, name, access_path, range
  - 便捷构造器：`parameter()`, `local()`, `field()`, `return_()`, `call_arg()`, `literal()`, `expr()` — 接受 `Option` 类型的 FK 字段
- `DataFlowEdge` — 数据流边
  - source/target 都是 `DataNodeId`（NOT SymbolId）
  - 字段：id, source, target, kind, location, confidence
- `CallsiteArg` — 调用点参数
  - 字段：callsite_id, index, name, expr_text, data_node, range
- 3 个测试

### P3-4: ReferenceUse 扩展 (`src/types/structs.rs`)

在 `ReferenceUse` 新增字段：
```rust
pub binding_id: Option<BindingId>,
```

更新了 15 个构造点（包括所有 6 个语言 adapter、store.rs、semantic_binder.rs）。

### P3-5: types/mod.rs 导出

新增导出：
- `pub mod bindings;`, `pub mod dataflow;`
- 所有新 ID 类型、枚举、结构体的 public export

### P3-6: DB Schema v5 (`src/db/schema.rs`)

**版本变更**: `CURRENT_SCHEMA_VERSION` 4 → 5

**edges → symbol_edges 重命名**:
- DDL: `CREATE TABLE IF NOT EXISTS symbol_edges`
- 4 个索引: `idx_symbol_edges_*`
- 测试断言更新

**"references" 表扩展**:
- 新增 `binding_id BLOB` 列（column 19, nullable）

**5 张新表** (所有 FK `ON DELETE SET NULL`):

| 表名 | 主键 | 关键列 | FK |
|------|------|--------|-----|
| `bindings` | binding_id | file_id, function_id, scope_id, kind, name, symbol_id, range | function_id→symbols, scope_id→scopes, symbol_id→symbols, file_id→files |
| `binding_uses` | binding_use_id | file_id, scope_id, binding_id, reference_id, name, range | binding_id→bindings, reference_id→"references", scope_id→scopes, file_id→files |
| `data_nodes` | data_node_id | file_id, function_id, kind, binding_id, callsite_id, name, access_path, range | function_id→symbols, binding_id→bindings, callsite_id→callsites, file_id→files |
| `dataflow_edges` | dataflow_edge_id | source, target, kind, location, confidence | source→data_nodes, target→data_nodes |
| `callsite_args` | callsite_id+index_ | name, expr_text, data_node_id, range | callsite_id→callsites, data_node_id→data_nodes |

**20 个新索引**：覆盖所有查询模式（按函数、按绑定、按数据节点、按调用点）。

### P3-7: Store API (`src/db/store.rs`, +357/-74 lines)

**清理**: 删除 v2→v3, v3→v4 inline migration 代码

**edges → symbol_edges**: 6 处 SQL 更新

**REFERENCE_SELECT 常量**: 新增 `binding_id` 列

**新增写入 API**:
- `insert_bindings(&self, bindings: &[BindingDef])`
- `insert_binding_uses(&self, uses: &[BindingUse])`
- `insert_data_nodes(&self, nodes: &[DataNode])`
- `insert_dataflow_edges(&self, edges: &[DataFlowEdge])`
- `insert_callsite_args(&self, args: &[CallsiteArg])`

**新增查询 API**:
- `find_bindings_by_function(function_id) -> Vec<BindingDef>`
- `find_binding_uses_by_binding(binding_id) -> Vec<BindingUse>`
- `find_data_nodes_by_function(function_id) -> Vec<DataNode>`
- `find_dataflow_edges_by_source(node_id) -> Vec<DataFlowEdge>`
- `find_dataflow_edges_by_target(node_id) -> Vec<DataFlowEdge>`

### P3-8: LexicalBinder (`src/extraction/lexical_binder.rs`, ~220 lines)

```rust
LexicalBinder::extract(
    adapter, ts_lang, root, source, source_bytes,
    file_id, file_path, scopes, symbols
) -> LexicalBindingResult { bindings, uses }
```

**工作流**:
1. 运行 `adapter.lexical_query()` 获取捕获
2. 匹配捕获名 → BindingKind (通过 adapter 的 `normalize_lexical()`)
3. 解析 scope_id 通过 `innermost_scope()`
4. 生成 deterministic BindingId (re-hash 当 scope 变化)
5. 为每个 BindingDef 创建 BindingUse

**核心辅助方法**: `innermost_scope()` — 在 scope 树中找到包含给定 range 的最内层 scope。

**支持的语言**: TypeScript（已实现 query + adapter），其他语言待扩展。

### P3-9: DataFlowBuilder (`src/extraction/dataflow_builder.rs`, ~240 lines)

```rust
DataFlowBuilder::extract(
    adapter, ts_lang, root, source, source_bytes,
    file_id, file_path, bindings, scopes
) -> DataFlowResult { nodes, edges }
```

**工作流**:
1. 运行 `adapter.dataflow_builder_query()` 获取捕获
2. 匹配捕获名 → DataNodeKind (通过 adapter 的 `normalize_dataflow_builder()`)
3. 创建 DataNode（分配/目标、值、返回、调用参数、字面量）
4. 构建 DataFlowEdge（Assign: 值→目标, FieldLoad: 接收者→字段）
5. FK 字段全部为 `None` — 解析推迟到 SemanticBinder

**关键设计决策**: 所有 FK (function_id, binding_id, callsite_id) 在提取阶段设为 `None`，因为此时对应的数据库记录尚不存在。解析由后续流水线阶段完成。

### P3-10: extract_file() 集成 (`src/extraction/extract.rs`)

在 extract_file() 流水线中插入两个新步骤：

```
extract_file(adapter, file_id, ...)
├─ 1. Parse with tree-sitter
├─ 2. Extract definitions (SymbolDef)
├─ 3. Extract references (ReferenceUse)
├─ 4. Extract imports (ImportDef)
├─ 5. Extract scopes (ScopeDef)
├─ 6. Extract raw dataflow (RawEdge)         [deprecated path]
├─ 7. Build scope tree
├─ 8. Lexical binding extraction      ← NEW  (P3)
├─ 9. DataFlow node/edge extraction   ← NEW  (P3)
├─ 10. SemanticBinder::bind_all()
├─ 11. Derive callsites
├─ 12. Collect exports
└─ 13. Assemble FileFacts (with P3 fields)
```

新步骤是非致命的：如果 language adapter 未实现 lexical/dataflow 查询（返回空字符串），则产生空的默认值。失败时产生 warning 并降级为空结果。

### P3-11: LanguageAdapter 扩展 (`src/extraction/languages/mod.rs`)

新增可选的 trait 方法：

```rust
fn lexical_query(&self) -> &str { "" }        // 默认空字符串
fn dataflow_builder_query(&self) -> &str { "" } // 默认空字符串
fn normalize_lexical(&self, capture: &str, text: &str, range: TextRange, file_id: &FileId) -> Option<BindingDef> { None }
fn normalize_dataflow_builder(&self, capture: &str, ...) -> (Option<DataNode>, Option<DataFlowEdge>) { (None, None) }
```

**TypeScript adapter 实现**:
- `lexical_query()` → `lexical.scm` (35 lines, 捕获参数/局部变量/import别名/catch变量/字段)
- `dataflow_builder_query()` → `dataflow_builder.scm` (41 lines, 捕获赋值/返回/调用参数/成员访问/字面量)
- `normalize_lexical()` — 映射 6 种 lexical.* 捕获到 BindingKind
- `normalize_dataflow_builder()` — 映射 8 种 df.* 捕获到 DataNode/DataFlowEdge

### P3-11b: Tree-sitter Queries (TypeScript)

**`src/extraction/queries/typescript/lexical.scm`** (35 lines):
- 参数: `(required_parameter (identifier) @lexical.parameter)`
- 局部变量: `(variable_declarator (identifier) @lexical.local)`
- Import 别名: `(import_specifier (identifier) @lexical.import_alias)`
- Catch 变量: `(catch_clause (identifier) @lexical.catch_variable)`
- 字段: `(public_field_definition (identifier) @lexical.field)`

**`src/extraction/queries/typescript/dataflow_builder.scm`** (41 lines):
- 赋值: `(assignment_expression left:(identifier)@df.assign_target right:(_)@df.assign_value)`
- 返回: `(return_statement (_) @df.return_value)`
- 调用参数: `(arguments (_) @df.call_arg)`
- 成员访问: `(member_expression object:(_)@df.receiver property:(property_identifier)@df.field_name)`
- 字面量: `(string)@df.literal`, `(number)@df.literal`, 等

---

## 5. 修复的关键问题

### 5.1 FK 约束违规

**问题**: DataFlowBuilder 创建了不存在的占位 ID（CallsiteId, BindingId, SymbolId），导致 INSERT 时 FK 约束失败。

**修复**:
- `DataNode` 便捷构造器参数改为 `Option<T>`：
  - `parameter(fid: Option<SymbolId>, bid: Option<BindingId>, ...)`
  - `local()` 同理
  - `return_(fid: Option<SymbolId>, ...)`
  - `call_arg(cid: Option<CallsiteId>, ...)`
- TypeScript adapter 的 `normalize_dataflow_builder()` 全部传 `None`
- 解析延期到 SemanticBinder 后处理

### 5.2 Tree-sitter Query 不匹配

**问题**:
- `(import_statement (identifier))` 不匹配 — import_statement 下没有直接的 identifier 子节点
- 解构模式 `(shorthand_property_identifier)` 等被 tree-sitter-typescript 嵌套在不同结构中

**修复**:
- 改用 `(import_clause (identifier))` + `(namespace_import (identifier))`
- 移除不工作的解构模式捕获
- 使用 `tree-sitter query` CLI 验证 query 模式

### 5.3 未使用变量/导入警告

**修复**: 移除未使用的 `BindingKind`, `CallsiteId`, `SymbolId`, `TextRange` 导入；移除未使用的 `collect_lexical_captures` 函数；标记 `mut` 变量。

---

## 6. 文件清单

### 修改的文件 (16)
| 文件 | 变更 | 说明 |
|------|------|------|
| `src/types/ids.rs` | +257 | 4 新 ID 类型 + 9 测试 |
| `src/types/enums.rs` | +181 | 3 新枚举家族 |
| `src/types/structs.rs` | +30 | ReferenceUse.binding_id + FileFacts P3 字段 |
| `src/types/mod.rs` | +25/-1 | 新模块导出 |
| `src/db/schema.rs` | +162/-x | v5: 5 新表 + edges→symbol_edges + 20 索引 |
| `src/db/store.rs` | +357/-74 | P3 write/query API + edges→symbol_edges + 移除 migration |
| `src/extraction/extract.rs` | +48 | LexicalBinder + DataFlowBuilder 集成 |
| `src/extraction/mod.rs` | +4 | 新模块声明 |
| `src/extraction/semantic_binder.rs` | +1 | binding_id 测试 |
| `src/extraction/languages/mod.rs` | +44 | LanguageAdapter 扩展 |
| `src/extraction/languages/typescript.rs` | +143 | lexical/dataflow 实现 |
| `src/extraction/languages/python.rs` | +1 | binding_id |
| `src/extraction/languages/java.rs` | +1 | binding_id |
| `src/extraction/languages/c.rs` | +1 | binding_id |
| `src/extraction/languages/cpp.rs` | +1 | binding_id |
| `src/extraction/languages/cangjie.rs` | +1 | binding_id |

### 新增的文件 (6)
| 文件 | 行数 | 说明 |
|------|------|------|
| `src/types/bindings.rs` | ~140 | BindingDef + BindingUse |
| `src/types/dataflow.rs` | ~270 | DataNode + DataFlowEdge + CallsiteArg |
| `src/extraction/lexical_binder.rs` | ~220 | LexicalBinder |
| `src/extraction/dataflow_builder.rs` | ~240 | DataFlowBuilder |
| `src/extraction/queries/typescript/lexical.scm` | 35 | TS 词法绑定 query |
| `src/extraction/queries/typescript/dataflow_builder.scm` | 41 | TS 数据流 query |

### 统计
- **22 个文件变更** (16 修改 + 6 新增)
- **~1900 行新增**
- **201 个测试通过**, 2 ignored

---

## 7. 数据流对比

### P2 数据流
```
extract_file()
  └─ SymbolDefs + ReferenceUses + RawEdges (symbol-level, deprecated)
       ├─ SemanticBinder (bind_source, bind_scope)
       └─ Store → symbol_edges table
```

### P3 数据流
```
extract_file()
  ├─ SymbolDefs + ReferenceUses
  ├─ RawEdges (symbol-level, deprecated path)
  ├─ LexicalBinder → BindingDefs + BindingUses     ← NEW
  ├─ DataFlowBuilder → DataNodes + DataFlowEdges   ← NEW
  ├─ SemanticBinder (bind_source, bind_scope)
  └─ Store → symbol_edges + bindings + binding_uses + data_nodes + dataflow_edges
```

### 核心区别

| 维度 | P2 (symbol_edges) | P3 (dataflow_edges) |
|------|-------------------|---------------------|
| 源标识 | SymbolId | DataNodeId |
| 目标标识 | SymbolId | DataNodeId |
| 表示 | 符号间关系 | 数据流关系 |
| 粒度 | 符号级 | 表达式级 |
| 用途 | 调用图/依赖分析 | 污点分析/数据流分析 |

---

## 8. 待办 (DEFERRED)

以下项按 YAGNI 原则推迟到 P4：

| 项 | 推迟原因 |
|----|---------|
| BindingGraph (per-function graph loading) | P4 CFG 构建时需要 |
| DataFlowGraph (per-function graph loading) | P4 跨过程分析时需要 |
| SemanticBinder 扩展 (bind_lexical) | 当前 FK 为 None，P4 需要时补充解析 |
| 非 TS 语言的 lexical/dataflow query | 当前仅 TS 实现，其他语言在 P4 逐个补充 |
| 跨文件 binding 解析 | P4 跨过程分析 |
| 带 FK 解析的 DataFlow 写入 | P4 需要时补充 |

---

## 9. Git 提交

```
feat: P3 binding + dataflow — types, schema v5, LexicalBinder, DataFlowBuilder, edges→symbol_edges
```

### 历史提交 (4 prior)
1. `feat: P0 architecture refactor`
2. `refactor: remove find_enclosing_* from adapters`
3. `feat: P1 productization`
4. `feat: P2 module resolution + call graph`
