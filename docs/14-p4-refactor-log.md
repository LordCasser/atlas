# P4: CFG + CfgBuilder 实施日志

> 2026-05-20 | 235 tests passed | schema v6

## 变更概要

P4 实现了**控制流图 (Control-Flow Graph, CFG)** 构建能力，为未来的跨过程数据流和污点分析提供 AST 级结构骨架。

## 架构决策

### BindingGraph + DataFlowGraph 推迟到 P5

根据 YAGNI 原则，P3 级别的 `BindingGraph` 和 `DataFlowGraph`（per-function in-memory 加载）推迟到 P5，因为:
- 当前没有消费者需要 per-function binding graph 查询
- P5 污点分析可以直接从 DB 加载 bindings + data_nodes + dataflow_edges
- 避免构建当前不必要的抽象

### FunctionSummary 推迟到 P5

P4 设计文档中的 `FunctionSummary`（input_flows/sources/sinks/sanitizers/side_effects）推迟到 P5，因为:
- CFG 已经提供了必要的结构骨架
- FunctionSummary 是 P5 污点分析的输出产物，不是 P4 的输入

### IntraproceduralDataflow / InterproceduralDataflow 推迟到 P5

这些是 P5 污点分析引擎的内部组件，P4 只提供 CFG 结构。

## 实施内容

### P4-1: CFG 类型层 (src/types/ids.rs + enums.rs + cfg.rs)

**新增 ID 类型:**
- `CfgNodeId` — blake3(function_id + kind + start_byte)
- `CfgEdgeId` — blake3(source + target + kind)

**新增枚举:**
- `CfgNodeKind` (8 值): Entry, Statement, Branch, Loop, Return, Throw, Join, Exit
- `CfgEdgeKind` (5 值): Normal, TrueBranch, FalseBranch, LoopBack, Exceptional

**新增结构体:**
- `CfgNode` — function_id, kind, stmt_range
- `CfgEdge` — source (CfgNodeId), target (CfgNodeId), kind

### P4-3: DB Schema v6 (src/db/schema.rs)

- Version: 5 → 6
- 新增表 `cfg_nodes`:
  - cfg_node_id BLOB PK
  - function_id BLOB FK → symbols(symbol_id) ON DELETE CASCADE
  - kind TEXT
  - stmt_range 6 列 (start_byte, end_byte, start_line, start_column, end_line, end_column)
- 新增表 `cfg_edges`:
  - cfg_edge_id BLOB PK
  - source BLOB FK → cfg_nodes(cfg_node_id) ON DELETE CASCADE
  - target BLOB FK → cfg_nodes(cfg_node_id) ON DELETE CASCADE
  - kind TEXT
- 5 个新索引: idx_cfg_nodes_function, idx_cfg_edges_source, idx_cfg_edges_target, idx_cfg_edges_source_target, idx_cfg_nodes_function_kind

### P4-4: Store API (src/db/store.rs)

**写入:**
- `insert_cfg_nodes()`, `insert_cfg_edges()`
- `insert_file_facts()` 扩展支持 CFG 字段

**查询:**
- `find_cfg_nodes_by_function()`
- `find_cfg_edges_by_source()`

**FileFacts 扩展:**
- `cfg_nodes: Vec<CfgNode>` — P4 CFG 节点
- `cfg_edges: Vec<CfgEdge>` — P4 CFG 边

### P4-5: CfgBuilder (src/extraction/cfg_builder.rs, ~350 lines)

**职责:** 从 tree-sitter AST 为每个函数构建 CFG。

**支持的控制流结构:**
- Block statements (顺序 Normal 边)
- if/else → Branch → TrueBranch/FalseBranch → Join
- for/while/do → Loop → LoopBack → 出口
- return/throw → Exit 连接
- expression_statement, variable_declaration → Statement 节点

**不支持 (推迟):**
- try/catch/finally
- switch/case
- async/await
- labeled break/continue

**API:**
```rust
pub fn build(function_id: &SymbolId, function_node: Node, source_bytes: &[u8]) -> CfgResult
```

**不变量:**
- 每个函数 CFG 有且仅有一个 Entry 和一个 Exit 节点
- 所有 CfgNodeId / CfgEdgeId 是确定性 blake3 哈希
- 所有节点属于同一个 function_id

### P4-7: extract_file() 集成 (src/extraction/extract.rs)

**新增 Step 7c — CfgBuilder:**
1. 过滤 `SymbolKind::Function | Method | Constructor` 的符号
2. 用 `descendant_for_byte_range` 定位每个函数符号名处的 tree-sitter 节点
3. 向上遍历 parent 链找到函数节点 (function_declaration 等)
4. 对每个匹配的函数调用 `CfgBuilder::build()`
5. 非致命失败生成 Warning diagnosis

**helper 函数:**
- `build_cfg_for_functions(root, symbols, source_bytes) → Result<CfgResult>`
- `find_function_node(root, symbol) → Option<Node>` — 通过 name_range 定位

### P4-8: Golden Tests

**新增 TS CFG fixture:** `tests/fixtures/typescript/cfg.ts`
- 2 个函数：greet (顺序 + return) 和 check (if/else)
- 预期: 8 nodes (2 entry + 2 exit + statement + return + branch + join), 6 edges

**GoldenExpected 扩展:**
- `cfg_nodes: Vec<GldCfgNode>` — kind
- `cfg_edges: Vec<GldCfgEdge>` — source_kind, target_kind, kind

**现有 fixture 更新:**
- `typescript/simple.expected.json` — 新增 CFG (12 nodes, 8 edges)
- `python/simple.expected.json` — 新增 CFG (9 nodes, 3 edges)
- `typescript/imports.expected.json` — 新增 CFG (14 nodes, 12 edges)
- `c/includes.expected.json` — 无函数符号，无 CFG

## 数据流

```
extract_file()
  │
  ├─ Step 2: extract_and_normalize(definitions) → Vec<SymbolDef>
  ├─ Step 7: build_scope_tree()
  ├─ Step 7a: LexicalBinder::extract() → bindings, binding_uses
  ├─ Step 7b: DataFlowBuilder::extract() → data_nodes, dataflow_edges
  ├─ Step 7c: [NEW] build_cfg_for_functions()
  │     ├─ filter symbols → Function | Method | Constructor
  │     ├─ find_function_node() → locate tree-sitter function node
  │     └─ CfgBuilder::build() per function → Vec<CfgNode> + Vec<CfgEdge>
  ├─ Step 8: SemanticBinder::bind_all()
  └─ FileFacts { ..., cfg_nodes, cfg_edges }
```

## 文件变更

| 文件 | 操作 | 行数 |
|------|------|------|
| src/types/ids.rs | 修改 | +60 (2 ID 类型 + tests) |
| src/types/enums.rs | 修改 | +50 (2 enum) |
| src/types/cfg.rs | 新建 | ~160 |
| src/types/mod.rs | 修改 | +5 |
| src/types/structs.rs | 修改 | +2 |
| src/db/schema.rs | 修改 | +80 (v6 DDL + indexes) |
| src/db/store.rs | 修改 | +120 (insert/query APIs) |
| src/extraction/cfg_builder.rs | 新建 | ~350 |
| src/extraction/mod.rs | 修改 | +2 |
| src/extraction/extract.rs | 修改 | +65 (integration + helpers) |
| src/types/bindings.rs | 修改 | -1 (unused import) |
| tests/fixtures/typescript/cfg.ts | 新建 | 10 |
| tests/fixtures/typescript/cfg.expected.json | 新建 | 自动生成 |
| tests/golden.rs | 修改 | +50 (CFG types + test) |
| tests/fixtures/*.expected.json | 更新 | 4 files (CFG 字段) |

## 约束验证

- [x] 不破坏现有 235 tests
- [x] CfgNodeId 确定性 (blake3)
- [x] FK 约束: cfg_nodes.function_id → symbols, cfg_edges.source/target → cfg_nodes
- [x] Non-fatal 错误处理: CFG 失败不影响提取
- [x] YAGNI: FunctionSummary, IntraproceduralDataflow, InterproceduralDataflow 推迟到 P5
