# Atlas 深度技术解析：从源代码到 Agent 可查询的语义知识图谱

Atlas 是一个本地优先的代码语义知识图谱引擎，专为 LLM Agent 设计。它用 tree-sitter 从源码中**确定性地**提取符号、作用域、引用、调用关系、数据流、控制流，存入本地 SQLite 数据库，再通过标准化的 MCP 协议暴露给 AI Agent 查询。

整个工作流可以概括为：

```
源代码 ──[解析/抽取]──▶ .atlas/atlas.db ──[查询]──▶ Agent 上下文
        tree-sitter 事实        SQLite 真相源         Markdown 输出
```

Atlas 由 14 个 Rust crate 组成，依赖方向严格无环。CLI、MCP Server、TUI 三个入口最终都走同一套 `IndexPipeline`，只是交互方式不同。

---


## 一、使用入口

Atlas 提供三种使用方式：CLI、MCP Server、TUI。它们最终都走同一套 `IndexPipeline`，只是交互方式不同。
### 1.1 CLI 入口

`atlas` 命令行工具提供以下子命令（定义在 `crates/atlas-cli/src/lib.rs` 的 `Commands` 枚举中）：

```
atlas index                        # 索引项目（默认 manifest；--analysis structural|full）
atlas sync                         # 增量同步（升级已有索引的精度层级）
atlas status                       # 显示项目索引状态
atlas doctor                       # 检查环境就绪性（SQLite FTS5、grammar 支持、schema 版本）
atlas files                        # 列出已索引文件
atlas mcp                          # 启动 MCP 服务器（feature-gated）
atlas                              # 无子命令：启动 TUI
```

CLI **不提供** `search`、`calls`、`trace` 等查询子命令——这些是 MCP 工具的能力，需要通过 MCP Server 或 TUI 访问。CLI 的定位是**索引管理**：创建、升级、检查索引状态。交互式查询通过 TUI（无参数启动）或 MCP Server（`atlas mcp`）完成。

`atlas index` 的关键参数：
- `--analysis manifest|structural|full`：索引深度，默认 manifest
- `--force-reindex`：允许低精度索引覆盖已有的高精度索引（受 Index Precision Guard 保护）
- `--include` / `--exclude`：glob 模式过滤

CLI 只负责解析参数、加项目锁、调用共享的 `IndexPipeline`，然后展示结果。


### 1.2 MCP Server 入口

#### MCP 协议与工具演进


v1.3.1 将 33 个旧工具合并精简为 **18 个工具**，所有工具使用短名（无 `atlas_` 前缀）。合并策略是参数化：

| 旧工具（33 个） | 新工具（18 个） | 合并方式 |
|----------------|----------------|---------|
| `open_project`, `status`, `files` | `project(action="open\|status\|files")` | 参数化 action |
| `symbol`, `context`, `usages` | `symbol(view="detail\|context\|usages")` | 参数化 view |
| `callers`, `callees`, `callgraph`, `neighbors` | `calls(direction="incoming\|outgoing\|both")` | 参数化 direction |
| `trace_point`, `trace_variable`, `trace_caller_path`, `trace_forward` | `trace(kind="point\|variable\|callers\|forward")` | 参数化 kind |

#### 18 个工具的完整分类

所有工具注册在 `make_all_tools()`（`crates/atlas-mcp/src/tools/mod.rs`），按能力分为 9 组：

**项目与索引（2 个）**——直接读写 Store，不需要 graph：
- `project(action="open|status|files")`：打开项目、查看索引状态、列出文件
- `index`：触发索引/重索引（manifest/structural/full）

**符号查询（2 个）**——`symbol` 需要 graph，`search` 不需要：
- `search`：按名称搜索符号（支持 scope 过滤、kind 过滤）
- `symbol(view="detail|context|usages")`：单符号详情/上下文/引用位置（所有 view 都通过 graph 构建，但 detail 和 usages 的数据来源是 Store，context 需要完整邻接表）

**图查询（5 个）**——需要加载 GraphSnapshot 到内存（`tool_requires_graph` 返回 true）：
- `calls(direction="incoming|outgoing|both")`：调用图查询（支持 depth 多跳、edge_kinds 自定义边类型）
- `explore`：符号档案——收集 calls/references/implements/extends 等多类边，含源码片段和推荐下一步查询
- `path`：两符号间最短路径（Dijkstra，边权重编码语义惩罚）
- `impact`：影响分析——BFS 双向遍历可达符号集（`semantic=true` 含 lifecycle/branch_diff）
- `file_dependencies`：文件级依赖图（manifest 模式直接读 DB，structural 模式触发 lazy extraction）

**源码追踪（1 个工具 4 种 kind）**——需要数据流/CFG，可能触发 lazy extraction：
- `trace(kind="point|variable|forward|callers")`：
  - `point`：解析源位置上下文（enclosing symbol、reference、scope）
  - `variable`：变量来源追溯（Slicer 向后 BFS + CrossFunctionBridge 跨函数跳转）
  - `forward`：前向调用链（source → target 的 call path）
  - `callers`：调用链溯源（回溯到最远 caller）

**语义分析（2 个）**——需要 CFG + EffectComposer，C/C++ 主要适用：
- `lifecycle`：字段生命周期分析（CFG 路径敏感，检测 use-after-free / double-free / missing-free）
- `branch_diff`：分支副作用差异比较（if/else 或 switch/case 的 sibling 分支不对称检测）

**语义扩展（2 个）**——管理语义分析的规则和注解：
- `domain_rules(action="add|list|delete|learn")`：管理生命周期分析的领域规则（alloc_fn/free_fn/owned_pattern/cleanup_fn）
- `fp_dispatches(action="add|list|delete")`：管理 C/C++ 函数指针分发注解（struct field → concrete target function）

**任务管理（4 个）**——后台任务生命周期：
- `tasks`：列出所有后台任务（可按 query_id 过滤）
- `task_status`：轮询单个任务状态（running/completed/failed + 进度百分比）
- `wait_for_task`：阻塞等待任务完成（timeout_secs 最大 300）
- `resume_task`：使用原 query_id 恢复查询，获取 lazy extraction 完成后的增强结果

**Graph 初始化时机**：`ToolRouter::tool_requires_graph()` 只对 `symbol`、`calls`、`path`、`explore`、`impact` 返回 true。其他工具（`search`、`trace`、`file_dependencies`、`lifecycle`、`branch_diff`、`domain_rules`、`fp_dispatches` 等）直接读 Store，MCP `initialize` 和 `tools/list` 不会触发 graph 构建——这对大型项目的启动速度至关重要。

#### 源码追踪处理流程

```
Agent 请求 → 解析位置/符号
    → 检查目标文件的 CapabilityMask
    → 如果 capability 不足：触发 Lazy Extraction
        → ClosurePlanner 计算依赖闭包
        → 后台 Job 提取数据流/CFG
        → 返回 PrecisionTier + AnalysisContract
    → 如果 capability 足够：
        → 加载 DataNode + CFGNode
        → Slicer BFS 向后追踪
        → CrossFunctionBridge 跨函数跳转
        → 返回追踪路径 + 置信度
```

#### 语义分析处理流程

```
函数请求 → CFG Builder 构建控制流图
    → DataFlow Builder 构建数据流图
    → EffectComposer 组合 CFG + DataFlow + Domain Rules
    → OwnershipContract 解释 RuleMatch 为语义效应
    → 路径敏感分析（生命周期状态机 / 分支差异比较）
    → 返回结构化 issue 列表
```


#### AnalysisContract


```json
{
  "analysis_contract": {
    "safe_conclusions": ["call_graph_available", "symbol_resolved"],
    "unsafe_conclusions": ["dataflow_not_budgeted", "cfg_partial"],
    "capability_summary": { "mask": 31, "best_capability": "structural" },
    "refinement_jobs": ["lazy_dataflow_for_function_x"]
  }
}
```

这让 Agent 能够：
1. 理解当前结果的局限性
2. 决定是否需要触发更深层分析
3. 向用户解释"为什么找不到某些引用"


#### QueryResume

- **`resume_task(query_id)`**：使用原工具参数和 `LazyWindow` 重新执行查询，返回完整增强结果
- **`Investigation`**：MCP session 级隐式调查上下文，分析类工具会根据 symbol、position 或 field focus 更新 active investigation，并把相关文件/符号和期望能力传给 lazy 调度器

---

### 1.3 TUI 入口

Atlas 提供基于 `ratatui` 的交互式终端界面。运行 `atlas` 不带子命令时即进入 TUI 模式。

**即时启动**：不再阻塞于 `ensure_index_before_tui`。启动后立即进入界面；如果数据库为空，后台自动运行默认 structural 索引。

**后台作业系统**：`JobManager` 在 worker 线程上执行搜索和 trace，`Esc` 键取消运行中的作业。

**Tab 面板**：符号详情页分为多个标签页（Overview → Callers → Callees → Peers → Source），支持键盘导航。

**状态栏**：始终显示当前 index mode（empty/manifest/structural/full/partial）和活跃 job 数量。

**损坏数据库恢复**：如果 `.atlas/atlas.db` 损坏或 schema 初始化失败，保留不可用的 DB 为 `.corrupt.<timestamp>` 备份，创建新的 schema 并运行默认 structural 索引，完成后才启动交互。

**搜索与 Lazy 集成**：`SearchSession` 封装 `Engine`，当 manifest 搜索结果为空时自动触发 lazy structural retry。搜索完成后状态栏自动刷新 index mode。支持 `includeFilePeers` 开关以跳过文件同级符号查询（更快、更小响应）。

---

## 二、文件发现与 tree-sitter 解析

### 2.1 Git-aware 文件发现



### 2.2 线程局部解析器池


```rust
// 每个 Rayon 工作线程复用自己的 parser，避免反复分配
thread_local! {
    static TL_PARSER: RefCell<Option<Parser>> = const { RefCell::new(None) };
}
```

每解析一个文件，线程只需要调用 `parser.set_language()` 和 `parser.parse()`，而不需要重新创建 parser 对象。在 146 个文件的 TypeScript 项目中，平均每个文件的解析时间仅为 3.6ms。


### 2.3 SCM 查询与七层提取

tree-sitter 解析产生一棵 CST（Concrete Syntax Tree）后，Atlas 用 **SCM 查询**（类似 CSS 选择器，但用于树结构）精确匹配感兴趣的节点。Atlas 在编译期通过 Rust 的 `include_str!()` 宏将 `.scm` 查询文件直接嵌入二进制，运行时零文件 I/O。

每种语言都有一套独立的查询文件，覆盖七层提取：

| 查询文件 | 提取的信息 | 用途 |
|----------|-----------|------|
| `definitions.scm` | 符号定义（函数、类、变量等） | 构建符号表 |
| `references.scm` | 引用使用（调用、类型引用等） | 确定符号间关系 |
| `imports.scm` | 导入/导出语句 | 跨文件引用解析 |
| `scopes.scm` | 作用域边界（文件、函数、块） | 作用域链构建 |
| `lexical.scm` | 词法绑定（参数、局部变量） | 变量定义-使用链 |
| `dataflow_builder.scm` | 数据流节点 | 数据流图构建 |
| `cfg_builder.scm` | 控制流节点 | CFG 构建 |

---

---

## 三、LanguageAdapter：从捕获到标准化事实


tree-sitter 的查询返回 `(捕获名, CST节点)` 元组，但不同语言的 grammar 使用不同的节点类型名（TypeScript 用 `function_declaration`，Rust 用 `function_item`，Java 用 `method_declaration`）。Atlas 的方案是 **Slot-based LanguageFrontend**：

```rust
pub struct LanguageFrontend {
    pub symbols: Box<dyn SymbolExtractorSpec>,
    pub references: Box<dyn ReferenceExtractorSpec>,
    pub imports: Box<dyn ImportExtractorSpec>,
    pub scopes: Box<dyn ScopeExtractorSpec>,
    pub callsites: Box<dyn CallsiteExtractorSpec>,
    pub lexical: Box<dyn LexicalBindingSpec>,
    pub dataflow: Box<dyn DataflowSpec>,
    pub cfg: Box<dyn CfgSpec>,
    pub capability: LanguageCapabilityProfile,
}
```

每种语言实现一套 trait，核心的归一化逻辑抽取到共享的 `SymbolDefBuilder`，消除了约 60% 的重复代码。


标准化后的所有事实汇聚为 `FileFacts` 结构，包含该文件内的全部符号、作用域、引用、导入、调用点、数据流节点和 CFG 节点。


每种语言声明自己支持的提取能力。`CapabilityMask`（u16 位掩码）用 6 个比特标记：

```
Bit 0: manifest      — 顶层符号
Bit 1: structural    — 完整符号/引用/调用点
Bit 2: call_edges    — 调用边解析
Bit 3: cfg           — 函数级 CFG
Bit 4: dataflow      — 过程内数据流
Bit 5: summaries     — 跨函数摘要
```

Atlas 支持 14 种语言，都达到了 DataflowInterproc 级别，但每种语言的置信度不同（Go 0.78、Java 0.75、C 0.73、Python 0.72、TypeScript/JS 0.60 等）。

---

## 四、LexicalBinder：作用域感知的变量绑定


LexicalBinder 的工作流程：

1. 运行语言的 `lexical_query()`，捕获所有绑定声明（参数、局部变量、导入别名、catch 变量、类字段）
2. 为每个绑定确定其所在的作用域——通过 `innermost_scope()` 找到**最小的完全包含该绑定的作用域**
3. 生成 `BindingId`：`blake3(file_id + scope_id + kind + name + start_byte)`——**确定性、可复现**
4. 在声明位置创建一个 `BindingUse`：声明点本身也是使用点（对数据流分析很重要）


对于独立的标识符引用（如 `console.log(x)` 中的 `x`），`build_reference_binding_uses()` 扫描所有 `(identifier)` 节点，排除声明位置，然后通过**作用域链向上查找**找到对应的绑定：

```
当前作用域 → 父作用域 → 祖父作用域 → ... → 文件作用域
```

这种作用域链查找算法保证了在存在变量遮蔽（shadowing）时，标识符总是解析到最近的同名绑定。

---

## 五、引用解析：从名字到实体


解析采用级联策略，按精度从高到低依次尝试：

**阶段 1: Builtin 过滤**
识别语言内置符号（`console`、`Math`、`print`、`len`、`printf` 等），不参与项目内部解析。

**阶段 2: 作用域局部精确匹配**
在引用的作用域中查找同名符号 → 置信度 1.0。

**阶段 3: 容器/类内部匹配**
如果引用在方法内，在方法所属的类中查找 → 置信度 1.0。

**阶段 4: 同文件精确匹配**
在同一文件的所有符号中按名称查找 → 置信度按匹配质量计算。

**阶段 5: 导入/引用链解析**
通过 import/include 语句追踪到其他文件 → 置信度 0.8。对于 barrel 文件的 re-export，递归追踪重导出链（最多 10 层）。

**阶段 6: 项目全局搜索 + 模糊匹配**
- 阶段 6A: 全项目同名搜索，按目录邻近度排序 → 置信度 0.6
- 阶段 6B: Levenshtein 模糊匹配（编辑距离 ≤ 2） → 置信度 0.4


**未解析的引用不会被删除**。它们的 `resolved_symbol_id` 字段在数据库中保持 NULL，通过一个部分索引加速查找：

```sql
CREATE INDEX idx_references_unresolved
    ON "references"(resolved_symbol_id) WHERE resolved_symbol_id IS NULL;
```

这保证了当新文件被加入索引后，之前未解析的引用可以被重新解析而不会丢失信息。


模糊匹配是整个解析阶段的性能瓶颈（占 72%）。Atlas 使用两个关键优化：

**Trigram 预过滤**：在计算完整编辑距离前检查是否共享三元组（trigram）。如果共享零个 trigram，编辑距离一定 ≥ 2，可以直接跳过。在 5000 符号的项目中将候选集从 O(N) 缩小到约 50 个。

**长度剪枝**：如果 `|len(A) - len(B)| > max_distance`，Levenshtein 距离的下界已经超过阈值，直接跳过。

**全局符号索引缓存**：`GlobalSymbolIndex` 在构建时将所有符号加载到内存的 HashMap 中，避免解析过程中的重复 SQLite 查询。

---

## 六、数据流图：追踪值的来源


DataFlowBuilder 通过 **AST 驱动的方式**（不依赖 SSA 转换）构建 per-function 的数据流图，生成六种边：

| 边类型 | 置信度 | 触发模式 | 示例 |
|--------|--------|---------|------|
| `Assign` | 0.85-0.95 | `variable_declarator` / `assignment_expression` | `x = expr` |
| `FieldLoad` | 0.80 | 访问路径分解 | `obj.prop` → `obj` |
| `FieldStore` | 0.90 | 赋值表达式左侧为字段 | `obj.prop = val` |
| `ArgToCall` | 0.75 | 同一 `callsite_id` 组的调用参数 | `foo(a, b)` → `a`, `b` |
| `Read` | 0.75 | 子表达式包容 | 表达式节点读取其内部的值节点 |
| `ReturnValue` | 0.85 | return 语句包含的值节点 | `return x + y` → `x`, `y` |

此外还有 Use-Def 链（同为 `Assign` 类，置信度 0.85）连接变量定义点与其后续使用点，按 `(function_id, binding_id, name)` 分组，组内按字节位置排序后形成定义→使用的链。


**节点去重**：当多个 tree-sitter 捕获产生同一位置的相同类型节点时，通过 `NodePosKey { start_byte, end_byte, DataNodeKind }` 去重，只保留最后一个。

**调用点分组**：对于 `ArgToCall` 边，使用 `callsite_id` 将同一调用的参数和目标分组——这在存在嵌套调用（如 `foo(bar(), baz())`）时尤为重要，避免参数被归属于错误的调用。

**访问路径解析**：对于 `obj.prop1.prop2` 这样的链式字段访问，`FieldLoad` 边通过 `base_name_from_access_path()` 提取根变量名，再通过 `parent_access_path()` 逐层剥离来精确定位。


对于大型项目，全量数据流分析可能耗时较长。Atlas 提供 **LazyDataflow** 模式：仅在 Agent 查询某个特定位置时才触发分析，并设置预算上限：

```
每个分析单元的节点上限: 2000
每个分析单元的边上限: 20000
超时限制: 25 秒
```

---

## 七、控制流图：理解程序结构


Atlas 的 CfgBuilder 是 **per-function** 的，通过递归遍历 AST 构建控制流图。

支持的节点类型：Entry、Statement、Branch、Loop、Return、Throw、Join、Exit。支持的边类型：Normal（顺序）、TrueBranch / FalseBranch（条件分支）、LoopBack（循环回边）。

以 if-else 为例，算法生成：

```
Entry → Branch → TrueBranch → ... (consequence) ... → Join
                  FalseBranch → ... (alternative) ... → Join
 Join → (后续语句)
```


CfgBuilder 通过 `CfgLanguageConfig` 配置每种语言的节点类型名：

- TypeScript/JavaScript: `statement_block` / `if_statement` / `for_statement`
- Rust: `block` / `if_expression` / `for_expression` / `loop_expression`
- C/C++: `compound_statement` / `if_statement` / `for_statement`
- Java: 额外支持 `enhanced_for_statement`

当前 CFG 构建**不包含**以下复杂结构：try/catch/finally、switch/case、async/await、标注 break/continue、Rust match。


CFG 和数据流图在 Atlas 中是**互补关系**：数据流图追踪值的传播，CFG 描述程序的控制结构。在 Trace 引擎中，两者配合使用：CFG 提供函数的整体结构上下文，数据流图提供精确的值追踪路径。

Atlas 的 CFG **不实现支配树或可达性分析**。CFG 纯粹是结构化的——它生成图节点和边，但不做死代码消除或 SSA 构造。

---

## 八、GraphBuilder 与 GraphSnapshot


当 ReferenceResolver 完成引用解析后，GraphBuilder 将 `(ReferenceUse, ResolvedTarget)` 对转化为语义边：

- 对 `Function/Method/Constructor` 的调用 → `EdgeKind::Calls`
- 对 `Class/Struct` 的调用（如 `new Foo()`） → `EdgeKind::Instantiates`
- 对 `Interface/Trait` 的调用 → `EdgeKind::Implements`
- 对 `Variable` 的调用（函数指针） → Dataflow BFS（深度 3）尝试解析实际目标函数
- 继承声明 → `EdgeKind::Extends`
- 接口实现 → `EdgeKind::Implements`
- 其他引用 → `EdgeKind::References`


当引用目标是一个变量（如 `void (*fp)(int)`），GraphBuilder 会执行一次过程内 BFS（深度 ≤ 3）：

1. 在引用位置找到 `CallTarget` DataNode
2. BFS 向后追踪，跟随 `Assign / Read / FieldLoad` 等入边
3. 检查到达的源节点是否对应一个 `Function` 符号
4. 如果找到，创建 `Calls` 边，但置信度乘以 0.9 的惩罚因子（因为是间接调用）


GraphBuilder 还会检测 21 种常见的回调注册模式（`pthread_create`、`setTimeout`、`addEventListener`、`subscribe` 等），创建 `RegistersCallback` 边（置信度 0.65）。

对于 Python 的 `@decorator` 语法，通过行号邻近度（20 行内）将装饰器与其修饰的函数关联（置信度 0.75）。


GraphSnapshot 是 Atlas 的"热路径"核心——所有图查询都在这个结构上进行，完全绕过 SQLite：

```rust
pub struct GraphSnapshot {
    pub nodes: Vec<NodeSummary>,           // 连续数组，O(1) 按索引访问
    pub edges: Vec<EdgeSummary>,           // 连续数组
    pub id_to_idx: HashMap<SymbolId, NodeIx>,   // O(1) 符号→节点映射
    pub name_index: HashMap<String, Vec<NodeIx>>,   // 按名称多索引
    pub qname_index: HashMap<String, Vec<NodeIx>>,  // 按完全限定名多索引
    pub file_index: HashMap<FileId, Vec<NodeIx>>,    // 按文件多索引
    pub edge_count: usize,
}
```

关键设计：
- **不可变性**：构建后不可修改，通过 `Arc` 安全共享
- **双向邻接表**：`nodes[ix].outgoing` 和 `nodes[ix].incoming` 同时填充
- **预计算**：测试文件标记在构建时计算，Dijkstra 中不需要重复评估
- **数据流边排除**：目标为临时 dataflow ID 的边在快照构建时被静默丢弃


Atlas 的路径搜索使用 **Dijkstra 算法**，边权重编码语义信息：

| 惩罚因子 | 最大惩罚 | 触发条件 |
|----------|---------|----------|
| 间接调用 | +1.0 | `Implements/Instantiates/RegistersCallback` 边 |
| 低置信度 | +0.5×(1−confidence) | confidence < 1.0 |
| 启发式来源 | +0.3 | Provenance 为 Heuristic/CallbackPattern |
| 测试文件 | +0.5 | 节点位于测试目录 |
| 边缘命名 | +0.5 | proxy/fallback/alt 等模式 |

**生产路径偏好**：`prefer_production = true` 时，测试文件节点额外获得 +5.0 惩罚，确保纯生产代码路径优先。

**多路径排名**：`k_ranked_paths` 按三维度评分：语义质量(40%) + 拓扑质量(35%) + 中心性(25%)。

---

## 九、函数摘要与跨函数追踪


Trace 引擎需要在函数之间跳转。为了避免每次追踪都重新提取数据流，Atlas 预计算 **per-function 摘要**，存入三张表：

| 摘要表 | 内容 | 用途 |
|--------|------|------|
| `summary_param_reaches` | 参数 P → 它流向的所有节点 | "这个参数最终影响到了什么地方？" |
| `summary_return_sources` | 返回值 R → 流向它的所有源节点 | "这个返回值依赖于哪些输入？" |
| `summary_call_arg_sources` | 调用参数 A → 它的上游数据来源 | "传给这个函数的参数值从哪里来？" |

摘要构建算法：加载函数内所有 DataNode → 构建前向邻接表 → 对每个参数节点 BFS 前向遍历，记录可达的调用参数、返回值和字段节点。


当 Slicer 在函数内追踪数据流到达一个 Parameter 节点时，需要继续向**调用者**追踪。

**入参桥接** (`incoming_for_param`)：

```
被调函数的 Parameter 节点
  → 查找谁调用了这个函数 (find_callsites_by_callee)
  → 匹配 arg_index == param_index
  → O(1) 查询 summary_call_arg_sources
  → 创建 ArgToParam 桥接边 (置信度 = row.confidence × 0.92)
```

**返回值桥接** (`incoming_for_call_result`)：

```
调用者的 CallReturn 节点
  → 解析 callsite → callee 符号
  → 加载 callee 的 DataNode，找到 Return 节点
  → O(1) 查询 summary_return_sources
  → 创建 ReturnToCall 桥接边 (置信度 = row.confidence × 0.85)
```

### 8.3 Slicer 向后 BFS

Slicer（数据流切片器）执行**向后 BFS**，跟随数据流的入边从 sink 回溯到 source。

跟随的边类型：`Assign / Read / Write / FieldLoad / FieldStore / ArgToCall / ArgToParam / ReturnValue / ReturnToCall / ReceiverToThis`。

**不跟随的类型**：`Phi`（控制流合并，仅值流）。

每步同时查询**真实边**（数据库）和**虚拟边**（CrossFunctionBridge 提供的跨函数边）。

Slicer 使用一种**两阶段 BFS** 策略保持 use-def 链的准确性：阶段 1 处理所有可行候选节点，阶段 2 只将最高优先级的候选节点入队。

置信度随追踪深度衰减：

```
confidence(depth, truncated)
  = max(1.0 - depth × 0.033, 0.3)      // 基础：每跳衰减 3.3%，最低 0.3
  = truncated ? max(base - 0.2, 0.1) : base  // 截断再加 0.2 惩罚
```

---

## 十、Lazy Indexing：按需触发的渐进式语义富化

### 9.1 四层提取模型

Atlas 将提取精度划分为四个层次：

| 层级 | 提取内容 | 耗时 |
|------|---------|------|
| `Manifest` | 仅顶层符号（函数、类、类型声明） | 毫秒级/文件 |
| `ResolutionSymbols` | 符号 + 导入/导出 + 作用域 | 轻量 |
| `Structural` | 完整符号、引用、调用点、调用图 | 秒级/文件 |
| `Dataflow/Full` | Structural + 数据流 + CFG + 函数摘要 | 秒级/函数 |

这四个层级通过 `extraction_state` 表记录每个文件的完成状态。`CapabilityMask`（u16 位掩码）用 6 个比特标记文件的能力。

### 9.2 PrecisionTier

当 Agent 查询一个文件时，Atlas 返回的不仅是结果，还有 **PrecisionTier**——告知 Agent 当前结果基于什么程度的事实：

| 精度层级 | 含义 | Agent 应如何处理 |
|----------|------|-----------------|
| `Exact` | 目标文件有完整 structural + dataflow，预算未超 | 结果可信 |
| `PartialExact` | structural 完整，但 dataflow 被预算截断 | 结果部分可信 |
| `DegradedStructural` | structural 预算超支，仅有 manifest | caller/callee 可能缺失 |
| `LocalDataflowOnly` | dataflow 仅对当前函数可用 | 跨函数追踪可能中断 |
| `ManifestOnly` | 仅顶层符号可用 | 无法回答引用相关问题 |
| `Unavailable` | 文件未索引或语言不支持 | 需要触发索引 |

**PrecisionTier 不是错误码**。`PartialExact` 仍然返回结果，但附带 `diagnostics` 和 `analysis_contract` 说明缺失的能力。

### 9.3 CancellationToken

Lazy 提取的预算约束已从"循环守卫"升级为"可中断提取"。

- **`CancelCheck` trait**：`fn is_cancelled(&self) -> bool`。`NeverCancel` 作为向后兼容的哨兵
- **6 个检查点（CP1-CP6）**：在 `parse()` 之前、符号查询后、引用查询后、导入/作用域查询后、DB 写入前、以及 `extract_file` 调用前插入取消检查
- **`LazyBudget`**：实现 `CancelCheck`（`is_cancelled = cancelled || time_exceeded`），超时时自动调用 `cancel()`

Cancellation 是**正常降级路径**，产生精度降级而非 MCP 工具错误。例如，当 dataflow 预算耗尽时，工具返回 `PartialExact` 层级并说明"dataflow 预算已耗尽"。

### 9.4 ClosurePlanner

当 Agent 要求分析某个文件时，这个文件可能依赖其他文件（如通过 `import` 或 `#include`）。`ClosurePlanner` 基于 import/include 图计算**依赖闭包**，确保被引用文件的 `resolution_symbols` 层先于主文件的 structural 层构建。

如果 `a.ts` 导入了 `b.ts` 中的符号，但 `b.ts` 只有 manifest 层，那么 `a.ts` 中对 `b.ts` 符号的引用将无法解析。ClosurePlanner 自动将 `b.ts` 加入 lazy 提取队列。

### 9.5 能力感知脏检查

传统的增量索引只比较文件内容哈希。Atlas 的能力感知脏检查更进一步：

> 一个文件只有在其**内容哈希未变**且**file-level `extraction_state` 的 complete capability 覆盖请求模式**时，才视为 clean。

这意味着如果文件 hash 未变但缺少 dataflow capability，当用户请求 `--analysis full` 时，该文件会被重新提取。支持 **manifest → structural → full 的无源码变更升级**。

---

## 十一、SymbolSelector：闭环符号解析引擎


当 Agent 查询"帮我分析 `resolve` 函数"时，一个项目中可能有多个同名函数。Atlas 的 SymbolSelector 是一个容错式符号解析引擎，它接受一个 JSON 结构：

```json
{
  "qualified_name": "atlas_engine::Engine",
  "file_path": "src/lib.rs",
  "line": 42,
  "kind": "function",
  "language": "rust"
}
```

`qualified_name` 是唯一必填字段，其余都是可选的**提示（hint）**。错误的提示不会阻塞正确匹配——只会影响候选排序。输出的 `file_path` 和 `line` 来自数据库事实，而非用户输入。


| 优先级 | 字段 | 精确得分 | 模糊得分 | 说明 |
|--------|------|---------|---------|------|
| P1 | qualified_name | +10,000 | — | 所有候选的基础分 |
| P2 | file_path | +3,000 | +2,000（后缀）/ +1,200（基名） | 后缀匹配 |
| P3 | line | +1,200 | +800（±2行）/ +500（±10行） | 容错排序 |
| P4 | kind | +200 | 0 | 弱 tiebreaker |
| P5 | language | +100 | 0 | 最弱信号 |

核心不变式：
- **不惩罚错误**：所有计分都是正向加成
- **唯一性阈值**：第 1 名与第 2 名分差 ≥ 400 时才视为唯一命中
- **始终返回实际值**：输出的 `file_path` 和 `line` 来自数据库事实


| 策略 | 多候选行为 | 适用工具 |
|------|-----------|---------|
| `UniqueOrCandidates` | 分差 ≥ 400 → 返回唯一结果；否则返回候选列表 | `symbol detail`, `explore`, `context` |
| `Aggregate` | 始终返回所有候选，图工具以所有候选为 roots 做并集 | `calls`, `impact`, `path` |
| `BestEffortSingle` | 始终选最佳，分差 < 400 时标记为 BestEffort | `trace`, `usages` |


当符号歧义无法消除时，Atlas 返回结构化候选列表：

```json
{
  "candidates": [
    { "symbol_id": "abc123", "name": "resolve", "kind": "function", "file_path": "src/a.rs", "score": 13200 },
    { "symbol_id": "def456", "name": "resolve", "kind": "method", "file_path": "src/b.rs", "score": 11800 }
  ]
}
```

Agent 可以：
1. 向用户确认"您指的是哪个 resolve？"
2. 直接使用 `symbol_id` 进行精确重试

`ScoredCandidate.symbol_ref` 是一个自包含的 `SymbolSelector`，可直接作为下一个查询的输入——实现**跨工具闭环**。

---

## 十二、多语言语义分析：Lifecycle 与 Branch Diff


在程序分析中，**Effect** 是指一个语句对程序状态产生的改变：

- `malloc(100)` → `Alloc` Effect
- `free(ptr)` → `Free` Effect
- `x = 10` → `Store` Effect
- `ptr = NULL` → `Nullify` Effect

传统的数据流分析只追踪**值**的流动，而语义效应分析还追踪**操作**的意图。


Atlas 的语义分析不是为每种语言硬编码规则，而是通过 **Domain Rules 通用化架构**实现：

```
CFG Builder + DataFlow Builder (extraction)
           │
           v
     EffectComposer (&dyn OwnershipContract)
           │
           v
     CfgNode.semantic_effects: Vec<SemanticEffect>
           │
    ┌──────┴──────┐
    v             v
branch_diff    lifecycle
```

| 组件 | 职责 | 语言专属？ |
|------|------|-----------|
| `domain_rules` 表 | 存储规则（pattern、kind、status） | 否 |
| `GenericRuleEngine` | 匹配规则（exact/prefix/suffix/glob/regex） | 否 |
| `LanguageRuleKinds` | 注册每种语言允许的 rule_kind | 是 |
| `OwnershipContract` | 解释 RuleMatch 为语义效应 | 是 |
| `EffectComposer` | 组合 CFG + DataFlow + 效应为语义结果 | 否 |

核心原则：`domain_rules` 表只存储，不解释；语义解释由 consumer 完成。


`OwnershipContract` 是语言实现的 trait，定义五种消费模式：

| 消费模式 | 说明 | 示例 |
|----------|------|------|
| `ExplicitCall` | 显式调用释放函数 | `free(ptr)` |
| `MethodCall` | 方法调用释放 | `file.close()` |
| `Implicit` | 隐式作用域退出清理 | C++ 析构函数、Rust `Drop` |
| `Deferred` | 延迟清理 | Go `defer`、Python `try/finally` |
| `ContextManaged` | 上下文管理器 | Python `with`、Java try-with-resources |

`ScopeExitAnalyzer` 统一处理所有语言的作用域退出：
- Rust `Drop` → 块边界 `Free` Effect
- C++ 析构函数 → 块边界 `Free`
- Python `with` / `__del__` → `ContextManaged` 清理
- React `useEffect` cleanup returns → `CleanupReturn` Effect
- Go `defer` → `Deferred` 清理


`FieldLifecycleEngine` 对字段状态做路径敏感分析，状态包括：

```
Unknown → MaybeLive → Assigned → Freed → Nullified → Escaped → Returned → Invalidated
```

- `Alloc` → `MaybeLive`
- `Store` → `Assigned`
- `Free` → `Freed`
- `Free` 后再次 `Free` → **DoubleFree**
- `Freed` 后 `Load` → **UseAfterFree**


`BranchDiffEngine` 比较 `if/else` 或 `switch/case` 的 sibling 分支的语义效应差异。如果 `if` 分支有 `Alloc` + `Free`，而 `else` 分支只有 `Alloc`，则报告不对称。

v1.4.0+ 的语义分支差异基于 `EffectComposition` 而非仅比较单个效应，输出结构化 `BranchDiffIssue`（含 asymmetry kind、confidence、evidence）。


- 主要适用语言：**C/C++**（通过 `CppOwnershipRules` consumer）
- 其他语言（Rust、Go、Python、Java、C#、Kotlin、TypeScript 等）已注册 domain rules，但语义解释 consumer 仍在逐步完善
- Atlas 不建完整跨函数 dataflow 全量分析来支撑 lifecycle，不建独立 Function IR；优先扩展 CFG/dataflow facts，让语义分析复用现有基础设施

---

## 十三、性能优化


- **线程局部 parser**：每个 Rayon 线程维护 tree-sitter Parser 实例，平均 3.6ms/文件
- **DataNode 去重**：`NodePosKey { range, kind }` 去重，减少 O(N²) 边构建
- **Lazy 窗口过滤**：按字节范围预过滤捕获，避免构建全文件数据流


- **WAL 模式**：SQLite 读写不互相阻塞
- **双连接架构**：写连接处理修改；只读连接 `PRAGMA query_only = ON` 服务查询
- **批量写入**：每批 50 条，单事务完成
- **大容量写入模式**：索引重建时临时禁用 fsync，提升缓存至 512MB


- **GraphSnapshot 预计算**：邻接表、多维度索引一次性构建，查询时纯 O(1) 访问
- **静态分发热路径**：`EdgeIterKind` 编译时决定遍历方向，消除 Dijkstra 热循环中的动态分发
- **OrdF64**：为 `f64` 实现 `Ord`，权重直接放入 `BinaryHeap` 做优先队列


v1.4.0 将 CLI、MCP、TUI 的索引逻辑统一为共享编排器：

| 编排器 | 替换的重复逻辑 | 大小缩减 |
|--------|--------------|---------|
| `IndexPipeline` | CLI index + TUI auto-index + MCP index | CLI: 509→274 行 (−46%) |
| `IncrementalPipeline` | CLI sync + MCP sync | SyncEngine: 328→145 行 (−56%) |

- **`ProgressSink` trait**：入口注入进度显示（终端进度条、MCP notification、no-op）
- **`JobContext`**：统一长任务上下文，捆绑 ProgressSink、取消令牌和可选 task ID


**TypeScript 项目（165 文件，1,704 符号，11,819 引用）**：

| 阶段 | 耗时 | 占比 |
|------|------|------|
| 文件发现 | 12ms | 0.1% |
| 解析/抽取 | 592ms | 6.4% |
| 数据库写入 | 2,190ms | 23.6% |
| 引用解析 | 5,957ms | **64.3%** |
| 图构建 | 496ms | 5.4% |
| **总计** | **9,264ms** | 100% |

**Atlas 自身（156 文件，11 种语言，5,065 符号，27,786 引用）**：

| 阶段 | 耗时 | 占比 |
|------|------|------|
| 解析/抽取 | 253ms | 0.9% |
| 数据库写入 | 2,207ms | 7.9% |
| 引用解析 | 22,300ms | **79.4%** |
| 图构建 | 3,367ms | 12.0% |
| **总计** | **28,100ms** | 100% |

**引用解析是绝对瓶颈**（占 64% ~ 79%）——模糊匹配占了其中 72% ~ 73%。内存最大 RSS 仅 176 MB。

---

## 十四、Index Precision Guard


v1.4.1 引入了 **Index Precision Guard**，防止意外降级已有索引：

- 当已有数据库是 `structural` 或 `full` 级别时，运行默认的 `manifest` 索引会**拒绝执行**，除非显式传入 `--force-reindex`
- `Store::read_index_mode()` 区分 8 种状态：`none`、`unknown`、`manifest`、`partial_structural`、`partial_structural+lazy`、`structural`、`structural+lazy`、`full`
- 这保护了 Agent 的交互体验：不会因为一次无意的 `atlas index` 调用而丢失 dataflow/CFG 能力


v1.4.1 之后的清理目标不是单纯减少行数，而是把重复实现压回稳定边界：

| 约束 | 含义 |
|------|------|
| **先删除，再抽象** | 零调用代码直接删除；只有当多个调用点共享不变式时才新增 helper |
| **入口层只做编排** | CLI/TUI/MCP 只解释参数、处理锁、进度、后台任务；dirty check、resolution、graph build 走共享入口 |
| **trait 默认实现只表达真正相同的规则** | 跨语言一致的校验可进入 trait default；语义差异必须在 registry 显式覆盖 |
| **MCP lazy envelope 只有一个构建路径** | 所有 lazy 响应通过 `LazyResponse` 统一注入字段，避免手写漂移 |
| **public facade 改造保留 ergonomics** | `atlas-engine` stable re-export 是外部契约；用 trait 替代 type alias 时必须提供兼容 wrapper |
| **测试支撑 API 不等同于死代码** | 仅测试使用的构造器必须通过 `pub(crate)`、`#[cfg(test)]` 明确标注 |

---

*文档版本：对应 Atlas v1.4.2*
