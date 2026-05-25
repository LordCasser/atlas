# Atlas 技术架构详解

## 目录

1. [概览：Atlas 是什么](#1-概览atlas-是什么)
2. [演进路径：从 tree-sitter 到 dataflow 追踪](#2-演进路径从-tree-sitter-到-dataflow-追踪)
   - [2.1 阶段零：tree-sitter 基础解析](#21-阶段零tree-sitter-基础解析)
   - [2.2 阶段一：结构化索引（Structural Index）](#22-阶段一结构化索引structural-index)
   - [2.3 阶段二：完整数据流分析（Full Dataflow）](#23-阶段二完整数据流分析full-dataflow)
   - [2.4 阶段三：惰性数据流（Lazy Dataflow）](#24-阶段三惰性数据流lazy-dataflow)
   - [2.5 阶段四：主动项目切换与临时内存工作区](#25-阶段四主动项目切换与临时内存工作区)
3. [当前技术栈局限](#3-当前技术栈局限)
4. [关键架构权衡](#4-关键架构权衡)
5. [技术栈总览](#5-技术栈总览)

---

## 1. 概览：Atlas 是什么

Atlas 是一个面向多语言的代码智能引擎，它为 AI 编程助手（MCP Client，如 Claude、Cursor 等）提供精确的代码理解和追踪能力。核心能力包括：

- **符号搜索** — FTS5 全文 + 模糊匹配查找函数、类、变量
- **调用图分析** — callers / callees / callgraph / path / impact
- **代码溯源** — `trace_point`（光标位置解析）、`trace_variable`（变量数据流追溯）、`trace_caller_path`（调用链追溯）
- **活跃项目切换** — MCP 运行时动态打开任意项目，支持内存/持久两种存储模式

Atlas 的独特之处在于：它不是简单的正则 grep 工具，也不是依赖于语言服务器（LSP）的封装。它在 **tree-sitter** 语法解析的基础上，构建了一套完整的 **结构化索引 → 引用解析 → 调用图 → 数据流分析** 管道，并且通过 **惰性数据流（Lazy Dataflow）** 机制，在日常查询时按需构建重量级分析数据，兼顾启动速度和追溯精度。

---

## 2. 演进路径：从 tree-sitter 到 dataflow 追踪

Atlas 的架构演进可以理解为三个阶段的数据粒度跃迁：

```
tree-sitter 解析         结构化索引              数据流分析
    (AST)          →    (符号 + 引用 + 调用边)  →  (数据节点 + 数据流边 + CFG)
  阶段零                阶段一                    阶段二 + 阶段三
```

### 2.1 阶段零：tree-sitter 基础解析

Atlas 的基石是 [tree-sitter](https://tree-sitter.github.io/)，一种增量式、容错性强的解析器生成框架。Atlas 内置了 15+ 语言的 tree-sitter 语法：

| 语言 | 前端 | 能力等级 |
|------|------|----------|
| TypeScript / JavaScript | `ts_callsite_extractor` | DataflowBasic |
| Python | `python_callsite_extractor` | DataflowBasic |
| Java | `java_callsite_extractor` | DataflowBasic |
| C / C++ | `c_callsite_extractor` | Symbolic |
| Go / C# / Rust / PHP / Ruby / Kotlin | 各语言前端 | Symbolic |
| ArkTS / Cangjie | 实验性支持 | Symbolic |

每个语言前端通过 **slot-based 接口** 连接 tree-sitter：

```rust
// 简化的前端接口概念
pub struct LanguageFrontend {
    pub parser_spec: ParserSpec,             // tree-sitter 语法对象
    pub symbol_extractor: SymbolExtractor,   // 如何从 AST 提取符号
    pub reference_extractor: ReferenceExtractor, // 如何从 AST 提取引用
    pub scope_extractor: ScopeExtractor,     // 如何提取作用域
    pub callsite_extractor: CallsiteExtractor,   // 如何识别调用点
    pub dataflow_spec: Option<DataFlowSpec>,    // (可选) 数据流查询
    // ...
}
```

该接口在各语言的具体实现中，将 tree-sitter 的 S-expression 查询映射到 Atlas 的统一 IR（中间表示）：

```
tree-sitter AST Node
    │
    ▼
LanguageFrontend (S-expression queries)
    │
    ▼
Atlas IR: FileFacts { symbols, references, scopes, data_nodes, ... }
```

### 2.2 阶段一：结构化索引（Structural Index）

**这是 Atlas 的默认模式，也是日常使用最多的模式。**

```
atlas index                      # 默认 structural
atlas index --analysis full      # 完整分析（阶段二）
```

在 Structural 模式下，`extract_file()` 使用 `ExtractionMode::Structural` 只执行以下阶段：

| 阶段 | 产出 | 用途 |
|------|------|------|
| parse | tree-sitter CST | 所有后续阶段的基础 |
| symbols | `SymbolDef`（函数、类、变量定义） | 符号搜索、跳转 |
| references | `ReferenceUse`（调用点、引用点） | usages 查询 |
| imports | `ImportDef`（import/include 语句） | dependencies/dependents |
| scopes | `ScopeDef`（作用域树） | 名称解析、trace_point |
| callsites | `Callsite`（调用表达式） | callers/callees |
| lexical_bindings (7a) | `BindingDef`（参数、局部变量声明） | 变量绑定 |
| semantic_bind (8) | 引用 → 符号初步关联 | 后续 resolution |

**不执行**的部分：data_nodes（数据节点）、dataflow_edges（数据流边）、use-def edges（使用-定义边）、CFG nodes/edges（控制流图）。

Structural 模式的速度快（通常秒级完成中等规模项目），产出的数据已经足够支撑：

- ✅ 符号搜索（`search`, `symbol`）
- ✅ 调用图分析（`callers`, `callees`, `callgraph`, `path`, `impact`）
- ✅ 位置解析（`trace_point`）
- ✅ 引用查询（`usages`, `dependencies`, `dependents`）

但 **不支持** `trace_variable`（变量数据流追溯），因为缺少 dataflow 数据。

**索引后，经过两个流水线阶段**：

1. **ReferenceResolver** — 三阶段引用解析
   - Stage 1: 作用域内精确匹配
   - Stage 2: Import/include 路径解析（支持 tsconfig path alias）
   - Stage 3: 项目级模糊名称回退（基于 `GlobalSymbolIndex` 内存索引）

2. **GraphBuilder** — 符号级调用图构建
   - 将已解析的 `(ReferenceUse, ResolvedTarget)` 转换为 `RawEdge`
   - 边类型：`Calls`, `Instantiates`, `Implements`, `Extends`, `References`, `Contains`
   - 使用 Rayon 并行构建

最终结果：一个完整的 **GraphSnapshot**（内存图快照），所有图查询在内存中完成，无 SQL 往返。

### 2.3 阶段二：完整数据流分析（Full Dataflow）

```
atlas index --analysis full
```

Full 模式执行 `ExtractionMode::Full`，在 Structural 的基础上额外启用：

| 阶段 | 产出 | 用途 |
|------|------|------|
| dataflow (7b) | `DataNode`（参数、局部变量、字段、返回值） | 数据流追踪 |
| use-def (7c) | `DataFlowEdge { kind: UseDef }` | 跨语句变量传播 |
| cfg (7e) | `CfgNode`, `CfgEdge`（控制流图） | 控制流分析 |
| ref_binding_uses (8a) | `BindingUse`（标识符使用点） | 精确使用-定义链接 |

**DataFlowBuilder** 是数据流分析的核心组件。它的工作流程：

```
1. 对每个函数执行 tree-sitter dataflow query
   → 捕获 AST 中的变量声明、赋值、字段访问、调用参数
   
2. normalize 捕获结果 → DataNode (id, kind, name, access_path, byte_range)

3. resolve_bindings_to_nodes → 将变量绑定到作用域链中的声明

4. resolve_dataflow_function_ids → 确定每个 DataNode 属于哪个函数

5. build_dataflow_edges → 构建边：
   ├── Assign edges: 赋值语句
   ├── FieldLoad edges: 字段访问（如 req.body.name）
   ├── ArgToParam edges: 调用参数 → 被调用函数形参
   ├── ReturnValue edges: return 语句 → 调用结果
   └── Read edges: 子表达式读取关系

6. resolve_use_def → 跨语句使用-定义边
   - 按 (function_id, binding_id, name) 分组
   - 第一个 Local/Parameter 视为定义，后续使用建边
   - 支持变量遮蔽（shadowing）检测
```

Full 模式的问题：**对大项目太慢**（数分钟），且生成的数据量大（数百万 data nodes + edges）。这对于 MCP 的实时交互场景不可接受。

### 2.4 阶段三：惰性数据流（Lazy Dataflow）

这是 Atlas 架构中最重要的创新。它解决了 "日常快 + 按需精确" 的矛盾：

```
日常索引：structural（快速）
    ↓
用户触发 trace_variable
    ↓
LazyDataflowPlanner 规划窗口 → LazyDataflowLoader 按需构建
    ↓
仅对查询相关的 2-64 个函数构建 dataflow
```

**架构组成**：

```
[lazy crate]
├── constants.rs     — 硬编码预算参数
├── planner.rs        — LazyDataflowPlanner
└── loader.rs         — LazyDataflowLoader

[extraction crate]
└── mode.rs           — ExtractionMode::LazyDataflow { window }
```

**LazyDataflowPlanner** 根据查询位置规划 "分析窗口"（`LazyWindow`）：

```
输入: (file_id, line, column)
    │
    ▼
1. 定位最内层引用 → 解析后的符号
2. 寻找包含该位置的最内层作用域 → 所在函数
3. 从种子函数出发，BFS 扩展 callers + callees（最大深度 2）
4. 截断策略: 最多 64 个 AnalysisUnit
    │
    ▼
输出: LazyWindow { units, truncated, variable_focus }
```

**LazyDataflowLoader** 对窗口中的每个 `AnalysisUnit` 执行 `get_or_build`：

```
对每个 unit:
  ├── 检查 analysis_artifacts 缓存
  │   ├── content_hash 匹配 → 缓存命中（跳过）
  │   └── content_hash 不匹配 → 缓存失效 → 重新构建
  │
  └── 重新构建:
      1. 从磁盘读取源文件
      2. 校验 content_hash（防止结构化索引过期）
      3. 调用 extract_file(ExtractionMode::LazyDataflow { window })
         → 仅对该 unit 所在文件，仅对窗口中的函数构建 dataflow
      4. 写入 DB: data_nodes, dataflow_edges, bindings, cfg
      5. 记录 artifact (content_hash, budget_exceeded, built_at)
```

**关键预算保护**（全部硬编码，不可配置）：

| 常量 | 值 | 作用 |
|------|-----|------|
| `LAZY_DATAFLOW_BUDGET_MS` | 25,000ms | 单次 lazy 操作的总时间预算 |
| `LAZY_DATAFLOW_MAX_DEPTH` | 2 | BFS 扩展最大深度 |
| `LAZY_DATAFLOW_MAX_UNITS` | 64 | 窗口最大 AnalysisUnit 数量 |
| `LAZY_MAX_NODES_PER_UNIT` | 2,000 | 单个 unit 最大 DataNode 数 |
| `LAZY_MAX_EDGES_PER_UNIT` | 20,000 | 单个 unit 最大 DataFlowEdge 数 |

超预算时，剩余 units 被跳过，`LazyWindow.truncated = true`，TraceEngine 返回 `partial_result: true` 并附带诊断信息，提示用户运行 `atlas index --analysis full` 获取完整覆盖。

**惰性构建的缓存机制**：每次构建后，在 `analysis_artifacts` 表中记录 `(file_id, unit_id, content_hash, budget_exceeded)`。后续查询同一文件（未修改）时直接命中缓存，避免重复解析。

### 2.5 阶段四：主动项目切换与临时内存工作区

这是最近完成的架构升级，使 Atlas MCP 不再强绑定启动时的单一项目。

**核心概念**：

```
MCP Server (单进程)
├── Active Project A → Store A (.atlas/atlas.db)
│   ├── index / search / trace 作用于 A
│   └── open_project(path="/repo/B", storage="memory")
│       ├── 创建内存 Store B
│       ├── 自动 init schema + structural index
│       └── activate_project() → 切换 active project 到 B
│
├── Active Project B → Store B (:memory:)
│   ├── search / trace 作用于 B
│   └── open_project(path="/repo/A", storage="persistent")
│       └── 切回 A（使用已有 .atlas/atlas.db）
│
└── 进程退出 → 内存 Store 自动释放
```

**设计决策**：

1. **不混合多项目索引** — 一个 Store 对应一个 Project Root。FileId 用相对路径确定，混入多 root 会造成 ID 冲突和源码读取错误。

2. **默认内存模式** — `open_project` 的 `storage` 默认值是 `"memory"`，零痕迹、无文件污染。只有显式 `storage: "persistent"` 才创建 `.atlas/atlas.db`。

3. **原子状态切换** — `activate_project()` 一次性替换 `store + project_root + lazy_service`，清除所有 graph/search/context 缓存。Mutex 保证切换期间的串行化安全。

4. **跨进程 FileLock** — persistent 模式索引时通过 SQLite `project_metadata` 表实现跨进程排他锁，防止 CLI 和 MCP 同时写入同一 DB。

---

## 3. 当前技术栈局限

### 3.1 语言支持深度的不均

Atlas 的语言前端能力分为四个等级：

| 等级 | 能力 | 语言 |
|------|------|------|
| **DataflowFull** | 完整 dataflow + CFG | (无) |
| **DataflowBasic** | 局部 dataflow（参数、局部变量、调用参数、返回值） | TypeScript, JavaScript, Python, Java |
| **Symbolic** | 符号 + 引用 + 调用图（无 dataflow） | C, C++, Go, C#, Rust, PHP, Ruby, Kotlin, ArkTS |
| **None** | 不支持 | Bash (实验性) |

这意味着：

- **只有 4 种语言**支持 `trace_variable`（变量数据流追溯）
- C/C++ 等系统语言只支持符号级查询，无法追踪变量流动
- 每个新语言的支持需要手动编写 tree-sitter S-expression 查询模板，工作量大

### 3.2 数据流分析的精度边界

即使对于 TypeScript/JavaScript/Python/Java，当前 dataflow 分析也有明确局限：

1. **文件内分析**（intra-procedural, single-file）
   - 跨文件数据流依赖解析后的调用边进行 `ArgToParam` 连接
   - 但不会自动追踪跨文件的数据传播（如一个模块导出变量 → 另一个模块使用）

2. **无类型推断**
   - 不进行类型推导（如 TypeScript 的类型窄化、Python 的类型标注）
   - 变量名追踪依赖**名称匹配 + 作用域链**，而非类型系统

3. **无跨函数数据流**
   - 函数内部的 dataflow 是完整的
   - 但 `f(x)` → `g(y)` 中，`x` 如何影响 `g` 的形参 `y`，只通过 `ArgToParam` 边连接
   - 不递归追踪被调用函数内部的数据传播

4. **使用-定义分析的简化**
   - `resolve_use_def` 按 `(function_id, binding_id, name)` 分组
   - 第一个匹配项视为定义，后续视为使用 — 无法处理复杂的分支重定义

5. **无别名分析**
   - `a = b; c = a;` 中，`b` → `c` 的间接关系不会被追踪
   - 只追踪直接赋值链（`a → b`, `c → a`）

### 3.3 惰性数据流的预算约束

惰性数据流通过硬编码预算防止 MCP 响应超时，但这带来的代价是：

- **`trace_variable` 可能返回部分结果**（`partial_result: true`）
- 预算不足时，用户必须运行 `atlas index --analysis full` 才能获得完整覆盖
- 25 秒的预算对大项目可能还不够，但增加预算又会影响 MCP 交互体验

### 3.4 结构化索引的保守策略

Structural 模式**不执行 dataflow**，这导致：

- 首次 `trace_variable` 调用时触发惰性构建，可能耗时数秒
- 缓存冷启动场景下（新打开项目），每次追溯都要重新构建
- 内存 DB 模式下，MCP 重启后所有惰性缓存丢失

### 3.5 内存 DB 的局限性

`open_project(storage="memory")` 的 Store 使用 SQLite `:memory:`，这意味着：

- 进程退出后数据完全丢失，每次重新打开项目都需要重建索引
- 无法与 CLI 共享状态（内存 DB 对其他进程不可见）
- 大项目的索引（即使 structural）可能消耗数百 MB 内存
- 单个 MCP 会话中反复切换项目时，旧项目的内存 Store 被释放（正常行为），但切换回去需要重新索引

### 3.6 无增量索引（目前）

当前 `atlas index` 是全量重建策略：

1. 发现所有文件
2. 删除旧数据（CASCADE）
3. 重新提取所有文件

对于大型 monorepo，即使只修改了一个文件，也需要重新索引全部文件。虽然有 `filesync` 模块的雏形，但尚未集成到 MCP 工作流中。

### 3.7 无多项目工作区

当前架构一个 MCP session 只能有一个 active project。虽然支持通过 `open_project` 动态切换，但不支持：

- 同时打开多个项目并在它们之间查询
- 跨项目的符号搜索或引用查询
- 项目间的依赖关系分析

---

## 4. 关键架构权衡

### 4.1 确定性 ID vs 自增 ID

**选择**：所有 ID（`FileId`, `SymbolId`, `ReferenceId`, `DataNodeId` 等）使用 `blake3(input_bytes)` 确定性生成。

```
FileId = blake3("src/main.ts")          // 路径哈希
SymbolId = blake3(file_id + language + path + kind)
ReferenceId = blake3(file_id + range + text + kind)
```

**权衡**：

| 优势 | 代价 |
|------|------|
| 相同输入 → 相同 ID，天然幂等 | ID 固定 32 字节（BLOB），比自增整数大 |
| 多进程可独立生成，无需协调 | 无法按插入顺序排序 |
| 跨 DB 复制/合并时 ID 不冲突 | 索引大小更大（BLOB 主键） |
| 冲突概率极低（256-bit） | FTS5 不能直接用 BLOB，需要额外处理 |

**为什么选这个方向**：Atlas 的设计场景中，CLI 和 MCP 可能独立对同一项目执行索引。确定性 ID 确保它们产生相同的数据，不必担心主键冲突。

### 4.2 结构化优先（Structural-First）vs 全量分析优先

**选择**：默认索引模式是 Structural（符号 + 引用 + 调用图），dataflow 按需构建。

**权衡**：

| 优势 | 代价 |
|------|------|
| 索引速度快（秒级 vs 分钟级） | 首次 trace_variable 有延迟 |
| 日常查询（search/callgraph）不受 dataflow 数据量影响 | 需要惰性数据流基础架构 |
| MCP 启动不阻塞 | 缓存管理复杂（content_hash 校验） |
| 数据库体积可控 | 无法提前发现所有可追溯路径 |

**为什么选这个方向**：MCP 的核心场景是 AI 助手进行代码理解和问答。大多数查询只需要 "这个函数被谁调用"（callers）或 "这个符号是什么"（symbol），不需要完整的数据流分析。只有少数场景（如 "这个变量的值从哪里来"）需要 dataflow。惰性按需构建精确匹配了这种访问模式。

### 4.3 内存图快照 vs 实时 SQL 查询

**选择**：`GraphEngine` 在首次使用时将 Store 中的符号和边加载到内存 `GraphSnapshot`，所有图查询在内存中完成。

**权衡**：

| 优势 | 代价 |
|------|------|
| 图查询 O(1) 邻居查找 + 有界 BFS/DFS | 大项目的内存占用 |
| 无 SQL 往返，交互延迟极低 | 切换项目后必须重建（`activate_project` 清缓存） |
| 支持复杂遍历（多跳、最短路径） | 索引变更后需要 `maybe_refresh_graph` 检测并重建 |

**为什么选这个方向**：MCP 交互对延迟敏感（AI 助手等待工具返回）。图查询（callers/callees/path/impact）在 SQL 中实现需要多次 JOIN 和递归 CTE，性能不可控。内存快照保证每次查询的延迟是确定的 O(depth × degree)。

### 4.4 单 Store 单项目 vs 多项目混合索引

**选择**：一个 SQLite 数据库对应一个项目。不同项目永远不会索引到同一个 DB。

**权衡**：

| 优势 | 代价 |
|------|------|
| FileId 基于相对路径，无冲突 | 无法跨项目搜索 |
| lazy loader 的 `root.join(path)` 始终正确 | 切换项目需要重建状态 |
| tsconfig path alias 是 project-scoped，不冲突 | 无法建立项目间依赖图 |

**为什么选这个方向**：多项目混合索引会带来根本性的路径歧义。`FileId::generate("src/index.ts")` 在 repo-A 和 repo-B 中产生**相同的 ID**，但它们是不同的文件。解决这个问题需要引入 project-id 作为 ID 前缀，导致整个数据模型和查询路径的全面重构。当前阶段，通过 `open_project` 的快速切换已经覆盖了 "在多个项目之间工作" 的需求。

### 4.5 同步工具调用 vs 异步/流式响应

**选择**：所有 MCP 工具调用是同步的（阻塞直到完成），包括 `open_project(index=true)` 和 `trace_variable`（含惰性构建）。

**权衡**：

| 优势 | 代价 |
|------|------|
| 实现简单，无并发复杂性 | 大操作可能超时 |
| 响应包含完整结果，客户端无需轮询 | `open_project` 索引大项目可能耗时数十秒 |
| 错误处理清晰 | 惰性构建超预算时返回部分结果 |

**为什么选这个方向**：MCP 协议本身是 JSON-RPC 请求-响应模型。引入异步操作需要额外的状态管理和回调机制，增加系统复杂度。通过预算保护（25s timeout）和惰性构建，当前方案已经在大多数场景下保持了可接受的响应时间。

### 4.6 进程内 LanguageFrontend 缓存 vs 按需创建

**选择**：`LazyDataflowLoader` 使用 `OnceLock<HashMap<Language, LanguageFrontend>>` 在进程生命周期内缓存所有语言前端。

**权衡**：

| 优势 | 代价 |
|------|------|
| 首次使用后零创建开销 | 启动时加载所有编译进的语言前端 |
| tree-sitter 语法对象复用 | 内存占用（tree-sitter 语法编译结果较大） |

**为什么选这个方向**：tree-sitter 的 `Language` 对象创建代价高（需要解析 `grammar.json`），但创建后是只读的、线程安全的。缓存是自然选择。

---

## 5. 技术栈总览

### 5.1 依赖拓扑

```
atlas-cli (CLI 命令入口)
├── atlas-mcp (MCP JSON-RPC 服务)
│   ├── protocol.rs (工具契约类型)
│   └── tools/ (工具处理器)
│       ├── open_project.rs (新增)
│       ├── index.rs (索引 + FileLock)
│       ├── status.rs (状态 + 存储模式)
│       ├── trace.rs (三种追溯)
│       ├── search.rs (符号搜索)
│       ├── graph.rs (图遍历：neighbors/callers/callees/callgraph/path/explore/impact)
│       ├── context.rs (AI 上下文构建)
│       ├── capability.rs (语言能力查询)
│       ├── usages.rs (引用查询)
│       ├── dependencies.rs (文件依赖)
│       └── dependents.rs (反向依赖)
│
└── atlas-engine (统一门面)
    ├── extraction (tree-sitter 解析 + 提取)
    │   ├── frontend.rs (LanguageFrontend slot 接口)
    │   ├── languages/ (15+ 语言具体实现)
    │   ├── dataflow_builder.rs (DataFlowBuilder)
    │   ├── lexical_binder.rs (词法绑定)
    │   ├── semantic_binder.rs (语义绑定)
    │   ├── symbol_registry.rs (符号注册表)
    │   └── worker.rs (并行提取池)
    │
    ├── resolution (引用解析)
    │   ├── import_resolver.rs (import/include 路径解析)
    │   ├── name_matcher.rs (名称模糊匹配)
    │   ├── context.rs (GlobalSymbolIndex)
    │   └── path_alias.rs (tsconfig path alias)
    │
    ├── graph (调用图)
    │   ├── graph_builder.rs (GraphBuilder + 并行建边)
    │   └── snapshot.rs (GraphSnapshot + 遍历算法)
    │
    ├── lazy (惰性数据流)
    │   ├── planner.rs (LazyDataflowPlanner)
    │   ├── loader.rs (LazyDataflowLoader + 缓存管理)
    │   └── constants.rs (预算参数)
    │
    ├── analysis (查询时分析)
    │   └── trace/
    │       ├── locator.rs (位置 → TracePoint)
    │       ├── slicer.rs (数据流逆向切片)
    │       └── caller_path.rs (调用链探索)
    │
    ├── db (SQLite 持久层)
    │   └── store/ (写入/读取接口 + 4 种 Reader trait)
    │
    ├── search (FTS5 + 模糊搜索)
    ├── context (AI 上下文 Markdown 构建)
    ├── filesync (增量同步 + FileLock)
    └── workspace (项目根 + .atlas/ 路径管理)
```

### 5.2 数据流（典型 trace_variable 路径）

```
MCP Client
    │
    ▼
AtlasMcpService::call_tool("trace_variable", { file_path, line, column, max_depth })
    │
    ├──[1] ensure_graph_initialized()    ← 首次需要，构建 GraphSnapshot
    │
    └──[2] ToolRouter::handle_trace_variable()
            │
            ├──[3] resolve_file_id()     ← 相对路径 → FileId
            │
            ├──[4] LazyDataflowService::ensure_for_position(file_id, line, column)
            │       │
            │       ├── Planner: 定位引用 → 作用域 → 种子函数 → BFS 扩展 → LazyWindow
            │       │
            │       └── Loader: for each AnalysisUnit:
            │               ├── get_artifact() → cache hit? → 跳过
            │               └── cache miss:
            │                    ├── read source from disk
            │                    ├── check content_hash → 结构化索引过期?
            │                    ├── extract_file(LazyDataflow { window })
            │                    │   └── DataFlowBuilder::extract()
            │                    │       ├── tree-sitter query → captures
            │                    │       ├── normalize → DataNodes
            │                    │       ├── resolve_bindings → 绑定到作用域
            │                    │       ├── build_dataflow_edges → Assign/FieldLoad/ArgToParam/...
            │                    │       └── resolve_use_def → 跨语句使用-定义
            │                    ├── store.replace_dataflow_for_unit()
            │                    └── store.upsert_artifact()
            │
            ├──[5] self.lazy_service 完成后:
            │       window.truncated? → partial = true, diagnostic 警告
            │
            ├──[6] RawTraceEngine::trace_variable(file_id, line, column, max_depth)
            │       │
            │       ├── Locator::locate() → TracePoint (symbol, data_node, scope, bindings)
            │       │
            │       └── Slicer::slice_backward() → TracePath
            │               └── for each step:
            │                    └── find_dataflow_edges_by_source(node_id)
            │                        ├── Assign edge → 上一赋值
            │                        ├── FieldLoad edge → 上一字段访问
            │                        ├── ArgToParam edge → 调用者参数
            │                        └── UseDef edge → 定义点
            │
            └──[7] TraceQueryResponse<TracePath> → JSON → MCP Client
```

### 5.3 关键数据表

```
SQLite Schema:

files               (file_id BLOB PK, path TEXT, language TEXT, content_hash TEXT, status TEXT)
symbols             (symbol_id BLOB PK, file_id BLOB FK, kind TEXT, name TEXT, qualified_name TEXT, ...)
references          (reference_id BLOB PK, file_id BLOB FK, kind TEXT, text TEXT, resolved JSON, ...)
scopes              (scope_id BLOB PK, file_id BLOB FK, kind TEXT, parent_id BLOB, range, ...)
imports             (import_id BLOB PK, file_id BLOB FK, module TEXT, imported_name TEXT, ...)
symbol_edges        (edge_id BLOB PK, source BLOB, target BLOB, kind TEXT, confidence REAL, ...)
callsites           (callsite_id BLOB PK, file_id BLOB FK, caller BLOB, callee BLOB, ...)
data_nodes          (data_node_id BLOB PK, file_id BLOB FK, function_id BLOB, kind TEXT, access_path TEXT, ...)
dataflow_edges      (edge_id BLOB PK, source BLOB, target BLOB, kind TEXT, ...)
bindings            (binding_id BLOB PK, file_id BLOB FK, scope_id BLOB FK, function_id BLOB, name TEXT, ...)
binding_uses        (use_id BLOB PK, file_id BLOB FK, binding_id BLOB, scope_id BLOB FK, name TEXT, ...)
cfg_nodes           (cfg_node_id BLOB PK, function_id BLOB, kind TEXT, ...)
cfg_edges           (edge_id BLOB PK, source BLOB, target BLOB, kind TEXT, ...)
analysis_artifacts  (file_id BLOB, unit_id BLOB, layer TEXT, content_hash TEXT, status TEXT, budget_exceeded BOOL, ...)
project_metadata    (key TEXT PK, value TEXT)
schema_versions     (version INTEGER, description TEXT, applied_at TEXT)

FTS5:
symbols_fts         (name, qualified_name)  -- content=symbols
```

---

*文档版本：与 atlas v0.1.0 保持一致*
