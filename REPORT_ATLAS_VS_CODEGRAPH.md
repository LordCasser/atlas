# Atlas vs CodeGraph 逐语言完整工具对比报告

> 生成日期: 2026-06-03 | Atlas v1.3.1 | 测试方式: 每项 MCP 工具逐一调用

---

## 1. C 语言 (curl)

### 项目概况
| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 732 | 755 |
| 符号数 | 11,276 | 11,187 |
| 边数 | 51,414 | 25,600 |
| 引用数 | 76,014 | - |
| Atlas 语言置信度 | 0.73 | - |
| CodeGraph 索引大小 | - | - |

### 工具全面测试

#### 1.1 atlas_search / codegraph_search

**测试场景**: 搜索 `curl_easy_perform`

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 搜索结果 | 1 个精确结果: `lib/easy.c:709` | 1 个结果: `lib/easy.c:710` |
| 排序 | 按 score 降序 (1.08) | 按符号名 |
| 噪音控制 | 精确，无额外结果 | 精确，无额外结果 |
| line 差异 | -1 (从 `{` 前一行开始计数) | - |

**测试场景**: 搜索 `main`

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 搜索结果 | 131+ 个 `main` 函数，按 score 排序 | 50+ 个 `main` (aggregated) |
| 聚焦能力 | 可指定 `scope` 限制范围 | 无精准 scope 过滤 |
| 区分度 | 显示完整路径，用户可判断 | 列出所有，无评分 |

**结论**: Atlas 搜索提供 score 排序和 scope 过滤，更适合大型项目

#### 1.2 atlas_symbol / codegraph_node

**测试场景**: 查看 `curl_easy_perform` 详情

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 定位行 | lib/easy.c:709 | lib/easy.c:710 |
| 返回格式 | 结构化 JSON | Markdown 文本 |
| 源码 | 含完整函数体 (含 caller/callee 摘要) | 含完整函数体 + trail |
| Callers 显示 | 190 个 callers (limit 控制) | 189+ 显示 "+179 more" |
| Callees | 1 个: `easy_perform` | 1 个: `easy_perform` |
| 程序化消费 | 适合 JSON 解析 | 适合人工阅读 |

#### 1.3 atlas_calls (incoming+outgoing) / codegraph_callers + codegraph_callees

**测试场景**: `curl_easy_perform` 调用图

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| Callers 数量 | 190+ (limit 分页) | 189+ (limit 10 仅显示前 10) |
| Callees 数量 | 1 (`easy_perform`) | 1 (`easy_perform`) |
| 边类型区分 | `calls`, `references` | 仅 `calls` |
| 深度控制 | `depth` 参数 | 单层，需手动 follow |
| 聚合 | 无，精确符号 | 多同名符号时聚合 (aggregated) |

**结论**: Atlas 在 C 语言调用图上更精确，支持边类型区分

#### 1.4 atlas_path / codegraph_trace

**测试场景**: `main → curl_easy_perform` 路径追踪

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 路径成功 | ✅ 成功 | ❌ 失败 |
| 路径质量 | "direct", score 0.925 | "No direct call path" |
| 歧义处理 | 从 131 个 `main` 中自动匹配 | 聚合 50 个 `main`，无法选择 |
| 失败原因 | - | 动态分派 / 歧义 |
| 路径展示 | Hop-by-hop 带 score | 聚合列表 |
| 多候选 | 1 个路径 + 1 个替代 | - |

**结论**: Atlas 在 C 路径追踪上显著优于 CodeGraph，歧义处理更强

#### 1.5 atlas_impact / codegraph_impact

**测试场景**: `curl_easy_perform` 变更影响分析

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 影响节点数 | 30 (depth 2) | 390 |
| 展示方式 | 文件分组，每个文件内符号列表 | 文件列表，含行号 |
| 覆盖范围 | 按 depth 精确限制 | 聚合所有调用者 |

**结论**: CodeGraph 的影响分析更全面（390 节点），但包含噪音；Atlas 更精确

#### 1.6 atlas_explore / codegraph_explore

**测试场景**: 探索 `curl_easy_perform` 的邻居

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 返回格式 | 结构化的 incoming/outgoing 边 | 关系图 + 源码 |
| 边类型 | `calls`, `references` | `calls`, `references` |
| 源码包含 | 不含 (需单独调用) | 含多个文件的 verbatim 源码 |
| 可读性 | 结构化数据 | 更友好 |

#### 1.7 atlas_file_dependencies (CodeGraph 无对应)

**测试场景**: `lib/easy.c` 的 include 依赖

- Atlas 返回 **38 个 include 依赖**
- 区分 `incoming` (哪些文件 include 本文件) 和 `outgoing` (本文件 include 了哪些)
- CodeGraph 无此工具

**结论**: Atlas 独特功能，对于 C/C++ 项目的 #include 分析非常有价值

#### 1.8 atlas_trace (callers) — Atlas 特有深度追踪

**测试场景**: 反向追踪 `curl_easy_perform` 的调用者链 (depth=3)

- 返回 **202 个节点** 的调用链
- 每步包含: 调用者/被调用者 hex ID、证据片段、参数值
- 截断提示: "Caller path truncated: reached depth 3 of max_depth=3"
- CodeGraph 无此功能

**结论**: Atlas 独有，适合深度的跨函数数据流分析

#### 1.9 atlas_lifecycle — Atlas C/C++ 特有

**测试场景**: 追踪 `curl_easy_perform` 中 `data->state` 的生命周期

- 返回 `incomplete` (该函数太短，仅代理调用)
- 但机制可用: CFG effect annotations, use-after-free/double-free 检测
- CodeGraph 无此功能

#### 1.10 codegraph_context — CodeGraph 独有

**测试场景**: "How does curl_easy_perform work?"

- 返回 `Entry Points`: `curl_easy_perform`, `CURL` type_alias
- 相关符号列表 (easy_perform, main 示例)
- 关键源码: `curl_easy_perform`, `easy_perform`, `CURL` typedefs
- Atlas 无此功能

#### 1.11 codegraph_files — CodeGraph 独有

**测试场景**: 项目文件结构

- 返回 755 个文件按语言分组 (C 724, yaml 12, cpp 6, ...)
- 每个文件附带符号计数
- Atlas 无 `atlas_files` 工具

### C 语言总结

| 维度 | 胜出 | 说明 |
|------|------|------|
| 符号搜索 | Atlas | score 排序 + scope 过滤 |
| 符号详情 | 平手 | Atlas JSON 适合程序化，CodeGraph Markdown 适合人工 |
| 调用图 | Atlas | 边类型区分 + 精确解析 |
| 路径追踪 | **Atlas** | 成功追踪 main→curl_easy_perform，CodeGraph 失败 |
| 影响分析 | CodeGraph | 节点更多 (390 vs 30) |
| 文件依赖 | **Atlas 独有** | include 依赖分析 |
| 深度追踪 | **Atlas 独有** | trace_callers 多跳分析 |
| 生命周期 | **Atlas 独有** | C/C++ CFG 分析 |
| 任务上下文 | **CodeGraph 独有** | codegraph_context |
| 文件浏览 | **CodeGraph 独有** | codegraph_files |
| 探索能力 | 平手 | 各有侧重 |

---

## 2. Go 语言 (gin) — 完整版

### 项目概况
| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 99 | 110 |
| 符号数 | 2,692 | 2,544 |
| 边数 | 17,579 | 7,196 |
| 引用数 | 25,645 | - |
| Atlas 语言置信度 | 0.78 (最高) | - |

### 工具全面测试

#### 2.1 atlas_search / codegraph_search

**场景 1**: 搜索 `Engine` 结构体

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 符号精度 | ✅ 正确: `gin.Engine.Engine` struct (gin.go:91) | ❌ 错误: 返回 `defaultValidator.Engine()` 方法 |
| Score | 1.06 | 无评分 |
| 数量 | 15 个相关结果排序 | 10 个 |
| 噪音 | 低，前几个都是相关 Engine 引用 | 中，含不相关 Engine 方法 |

**场景 2**: 搜索 `gin.Default`

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 结果 | 15 个结果，`gin.Default` 排第一 (score 0.68) | 10 个，聚合了多个 `Default` |
| 准确度 | ✅ 正确识别 | ⚠️ 聚合同名不同包符号 |
| 额外信息 | 显示 Default, DefaultQuery, DefaultPostForm 等 | 显示 Default, DefaultFileSystem 等 |

**结论**: Atlas 的 Go 符号解析精度显著高于 CodeGraph

#### 2.2 atlas_symbol / codegraph_node

**场景**: 查看 `gin.Default` 详情

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 定位 | gin.go:235 | `codegraph_node` 返回 "Symbol not found" |
| 源码 | ✅ 完整 6 行函数体 | ❌ 无法定位 |
| 签名 | `(opts ...OptionFunc) *Engine` | N/A |
| Callers | 7 个 (Bind, ShouldBind, 5 个测试) | N/A |
| Callees | 6 个 (Logger, New, Use, With, Recovery, debugPrintWARNINGDefault) | N/A |

**结论**: CodeGraph 的 Go 符号解析明显弱于 Atlas。`gin.Default` 在 `codegraph_node` 中找不到，而 `codegraph_search` 能搜到但聚合多个 Default。

#### 2.3 atlas_calls / codegraph_callers + codegraph_callees

**场景**: `gin.Default` 的调用关系

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| Callees 精确度 | **6 个精确** | 聚合结果含无关 Default |
| 边类型 | `calls` + `references` 区分 | 仅 `calls` |
| 调用者 | 7 个 (含测试) | 聚合后难以区分 |

**结论**: Atlas 的 Go 调用图精确度远超 CodeGraph

#### 2.4 atlas_path / codegraph_trace

**场景**: `gin.Default → ServeHTTP` 路径追踪

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 结果 | ❌ 失败 | ❌ 失败 |
| 失败原因 | Go 接口动态分派 (`*Engine` 方法集) | 接口方法分派 |
| 信息质量 | 提供 frontier 边界信息 | 仅说 "dynamic dispatch" |

**结论**: 两者都无法处理 Go 接口动态分派

#### 2.5 atlas_explore / codegraph_explore

**场景**: 探索 `gin.Default` 的邻居

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| Incoming | 9 条 (7 calls + 2 references) | 106 符号/31 文件 |
| Outgoing | 10 条 (6 calls + 4 references) | 关系图 + 源码 |
| 边类型标注 | ✅ 每条边标注 `calls` 或 `references` | ⚠️ 不区分 |
| 源码包含 | 不含 | ✅ 含多个文件 verbatim 源码 |

**CodeGraph explore 特点**: 返回 verbatim 源码（无需 Read 工具再读），适合一次性理解模块上下文。

#### 2.6 atlas_impact / codegraph_impact

**场景**: `gin.Default` 变更影响分析 (depth=2)

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 影响节点数 | 30 | **45** |
| 分组方式 | 文件分组 + 每个符号列表 | 文件 + 符号列表 |
| 覆盖范围 | 调用者 + 内部 callees | 内部 callees + ginS 全局实例包装器 |
| 额外覆盖 | 无 | **ginS/gins.go 全部 27 个包装函数** |

**结论**: CodeGraph 的 impact 分析更全面，覆盖到了 ginS 单例包装器的全部方法

#### 2.7 atlas_trace (callers) — Atlas 特有

**场景**: 反向追踪 `gin.Default` 的调用者链 (depth=3)

- 返回 **16 个节点** 的调用链
- 找到 `MustBindWith → ... → TestRaceParamsContextCopy → Default` 路径
- 每步包含: 调用参数、证据片段、文件位置
- 截断提示: "Caller path truncated: reached depth 3 of max_depth=3"
- CodeGraph 无此功能

#### 2.8 atlas_file_dependencies (Atlas 特有)

**场景**: `gin.go` 的 import 依赖

- Atlas 返回 **0 个依赖**（Go 的 import 解析可能未捕获）
- 说明 Go 的 import 分析在 Atlas 中尚未完善

#### 2.9 codegraph_context (CodeGraph 特有)

**场景**: "How does gin.Default create a router?"

- 返回 `Entry Points`: `gin.Default` (gin.go:236), `binding.Default` 等
- 关键源码: `Default`, `New`, `Engine` struct, `debugPrintWARNINGDefault`
- 发现 3 个同名的 `Default` 函数（gin.Default, binding.Default x2）
- Atlas 无此功能

#### 2.10 codegraph_files (CodeGraph 特有)

**场景**: 项目文件结构

- 返回 **110 个文件**，按 go (99) 和 yaml (11) 分组
- 每个文件包含符号计数
- Atlas 无等价功能

### Go 语言总结

| 维度 | 胜出 | 说明 |
|------|------|------|
| 符号搜索 | **Atlas** | 正确识别 Engine 而非 defaultValidator.Engine() |
| 符号详情 | **Atlas** | CodeGraph 的 codegraph_node 无法找到 gin.Default |
| 调用图 | **Atlas** | 6 个精确 callees，边类型区分 |
| 路径追踪 | 平手 | 均无法处理 Go 接口动态分派 |
| 影响分析 | CodeGraph | 覆盖更广（45 vs 30），含 ginS 包装器 |
| 深度追踪 | **Atlas 独有** | trace_callers 链式分析 |
| 文件依赖 | Atlas 独有 | 但 Go 支持待完善 |
| 任务上下文 | **CodeGraph 独有** | codegraph_context 含多个同名函数 |
| 文件浏览 | **CodeGraph 独有** | codegraph_files 语言分组 |
| 探索 | 各有侧重 | Atlas 边类型精确；CodeGraph 含源码 |
| 语言置信度 | Atlas: 0.78 | Atlas 对 Go 支持最佳 |

---

## 3. Rust 语言 (bat)

### 项目概况
| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 104 | 128 |
| 符号数 | 3,528 | 2,608 |
| 边数 | 15,030 | 5,160 |
| 引用数 | 32,427 | - |
| Atlas 语言置信度 | 0.70 | - |

### 工具全面测试

#### 3.1 atlas_search / codegraph_search

**场景 1**: 搜索 `PrettyPrinter` struct

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 结果 | `PrettyPrinter` (src/pretty_printer.rs:37) | `PrettyPrinter` (src/pretty_printer.rs:38) |
| Score | 1.06 | 无评分 |
| 额外 | - | +34 个成员方法列表 |

**场景 2**: 搜索 `Config` struct

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 结果 | `Config` (src/config.rs:36) | `Config` (src/config.rs:37) + 7 callers |
| Score | 1.06 | 无评分 |
| 额外 | 无 | **显示所有 7 个调用者** |

**场景 3**: 搜索 `print_with_writer` 方法

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 结果 | 找到 (qualified: `PrettyPrinter<'a>::print_with_writer`) | 找到 (simple: `PrettyPrinter::print_with_writer`) |
| 命名方式 | **需要 lifetime 泛型参数** `<'a>` | 简单命名即可 |
| 发现难度 | 高 (需知道精确的带有 lifetime 的 qname) | 低 |

**结论**: CodeGraph 的 Rust 搜索更易用（简化命名），Atlas 的 lifetime 限定更精确但使用不便

#### 3.2 atlas_symbol / codegraph_node

**场景**: 查看 `PrettyPrinter` 详情

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 结构体字段 | 7 个字段 | 7 个字段 |
| 成员方法 | **仅结构体定义** | **35 个成员方法列表+签名** |
| 源码 | 含结构体定义 | 含结构体定义 + 方法签名 |
| Callers | 0 (struct 级别) | 有 trail 显示调用关系 |

**结论**: CodeGraph 的 Rust 节点信息更丰富，自动列出 impl 块中所有方法

#### 3.3 atlas_calls / codegraph_callers + codegraph_callees

**场景**: `PrettyPrinter::print_with_writer` 的调用图

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| Atlas 所需命名 | `PrettyPrinter<'a>::print_with_writer` | `PrettyPrinter::print_with_writer` |
| Callees (Atlas) | **7 个**: clear, insert, HighlightedLineRanges, new, from, collect, Controller::run | **2 个**: print_with_writer, Result (type_alias) |
| 边类型 | `calls` + `references` + `instantiates` | 仅 `calls` |
| 丰富度 | Atlas 更多 callee (含 struct 初始化) | CodeGraph 较少 |

**结论**: Atlas 在 Rust 调用图上更完整，区分 calls/references/instantiates 三种边类型

#### 3.4 atlas_path / codegraph_trace

**场景 1**: `main → PrettyPrinter<'a>::print` 路径

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 路径成功 | ✅ 成功 | ❌ 失败 |
| 路径 | main (examples/advanced.rs) → PrettyPrinter::print → print_with_writer | - |
| 歧义处理 | 从 14 个 main 中匹配 | 聚合 21 个 main |
| Score | 1.0 (direct) | - |

**场景 2**: `Controller::run_with_error_handler` 符号解析

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 查询结果 | Symbol not found | ✅ 找到 |

#### 3.5 atlas_impact / codegraph_impact

**场景**: `PrettyPrinter<'a>::new` 变更影响

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 影响节点数 | **25** | **13** |
| 分组方式 | 文件分组 | 文件分组 |

**结论**: Atlas 影响分析覆盖范围更大 (25 vs 13)

#### 3.6 atlas_explore / codegraph_explore

**场景**: 探索 `PrettyPrinter<'a>::print_with_writer`

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| Incoming edges | 2 (print: calls + references) | 147 符号/25 文件 |
| Outgoing edges | 60+ (calls, references, instantiates) | 关系图: implements, references, calls |
| 边类型标注 | ✅ 精确区分 | ✅ 精确区分 |
| 源码 | 无 | ✅ verbatim 源码 |

#### 3.7 atlas_trace (callers) — Atlas 特有

**场景**: 反向追踪 `PrettyPrinter<'a>::print_with_writer` (depth=5)

- 返回 **33 个节点** 的调用链
- 找到 `App::new → ... → print → print_with_writer`
- 每步含: caller/callee hex ID、参数值、证据片段
- CodeGraph 无此功能

#### 3.8 atlas_file_dependencies (Atlas 特有)

**场景**: `src/pretty_printer.rs` 的依赖

- 4 个 use 依赖: Read, Path, Term, PagingMode
- 0 个被依赖

#### 3.9 codegraph_context (CodeGraph 特有)

**场景**: "How does bat's PrettyPrinter work?"

- 返回 `Entry Points`: `main` (src/bin/bat/main.rs:454), `Printer` (trait), `PrettyPrinter` (struct)
- 返回 15+ 相关符号
- 含关键源码: main, run, Printer trait, PrettyPrinter struct
- Atlas 无此功能

#### 3.10 codegraph_files (CodeGraph 特有)

**场景**: 项目文件结构

- **128 个文件**，17 种语言分组
- 主语言: rust (67 个文件)，其他含 yaml (8), python (7), ruby (5) 等测试文件
- 每个文件含符号计数

### Rust 语言总结

| 维度 | 胜出 | 说明 |
|------|------|------|
| 符号搜索 | CodeGraph | 简化命名，无需 lifetime 泛型 |
| 符号详情 | **CodeGraph** | 自动列出 35 个成员方法 |
| 调用图 | Atlas | 7 callees vs 2，边类型更丰富 |
| 路径追踪 | **Atlas** | 成功追踪 main→print，CodeGraph 失败 |
| 影响分析 | Atlas | 25 vs 13 节点 |
| 深度追踪 | **Atlas 独有** | trace_callers 跨 5 跳追踪 |
| 文件依赖 | Atlas 独有 | use 依赖分析 |
| 任务上下文 | **CodeGraph 独有** | codegraph_context |
| 文件浏览 | **CodeGraph 独有** | codegraph_files |
| 探索 | CodeGraph | 含 verbatim 源码，免 Read |

**特别注意**: Atlas 在 Rust 中方法查找需要使用带 lifetime 的 qualified name（如 `PrettyPrinter<'a>::print_with_writer`），对用户不友好。CodeGraph 使用简化名称即可。

---

## 4. TypeScript (opencode)

### 项目概况
| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 1,931 | 1,966 |
| 符号数 | 35,080 | 28,865 |
| 边数 | 65,350 | 66,375 |
| 引用数 | 315,356 | - |
| Atlas 语言置信度 | **0.60** (较低) | - |
| CodeGraph 节点类型 | - | class 402, function 5961, interface 508 |
| 数据库大小 | - | 52.19 MB |

### 工具全面测试

#### 4.1 atlas_search / codegraph_search

**场景**: 搜索 `ToolDefinition`

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 结果 | 2 个: class (messages.ts:151), type_alias (tool.ts:54) | 10 个结果含 func 引用 |
| Score | 1.06 / 1.0 | 无评分 |
| 额外 | - | 显示引用该符号的函数 |

**场景**: 搜索 `Config` — Atlas 找到 5 个 interface

**场景**: 搜索 `startServer`, `invokeModel` — Atlas 均无结果，CodeGraph 也无

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 覆盖 | 部分符号无法搜索到 | 搜索正常 |
| 可能原因 | TypeScript 置信度 0.60 导致索引不全 | - |

#### 4.2 atlas_symbol / codegraph_node

**场景**: 查看 `ToolDefinition` (class) 详情

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 定位 | messages.ts:151 | tool.ts:54 (type_alias) |
| 源码 | ✅ 含完整类定义 | ✅ 含完整 type 定义 |
| 歧义处理 | 返回特定 class | 聚合 2 个 ToolDefinition |
| Callers | 0 (class 级别) | 无 |

**结论**: CodeGraph 在有同名符号时聚合结果显示，可能导致混淆

#### 4.3 atlas_calls / codegraph_callees

**场景**: `ToolDefinition` 调用关系

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| Callees | 0 (class 级别) | 0 (aggregated) |
| Callers | 0 | aggregated 2 符号 |
| 边类型 | 仅 `references` (通过 explore) | 仅 `calls` |

#### 4.4 atlas_explore / codegraph_explore

**场景**: 探索 `ToolDefinition` 的邻居

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| Incoming | 8 references (Tool, make, fromPlugin 等) | **182 符号/48 文件** |
| Outgoing | 10 references | 关系图 + verbatim 源码 |
| 边类型 | `references` | `references`, `calls`, `instantiates`, `extends` |
| 源码 | 无 | ✅ 含 messages.ts, tool.ts, plugin/tool.ts 等 verbatim 源码 |

**CodeGraph explore 优势**: 一次性返回多个相关文件的 verbatim 源码，相当于完成了多次 Read 调用

#### 4.5 atlas_impact / codegraph_impact

**场景**: `ToolDefinition` 变更影响分析

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 影响节点数 | **1** (仅有自身) | **33** (含 14 个文件) |
| 覆盖 | 极有限 | 覆盖: markLastTool, make, toDefinitions 等 LLM 协议层 |
| 歧义 | - | 聚合 2 个 ToolDefinition |

**结论**: CodeGraph 在 TS 的影响分析远超 Atlas，Atlas 的 TS 支持明显不足

#### 4.6 atlas_file_dependencies (Atlas 特有)

**场景**: `packages/llm/src/schema/messages.ts` 的 imports

- **13 个 import 依赖** (effect, ./ids, ./options 等)
- 0 个被依赖

#### 4.7 codegraph_context (CodeGraph 特有)

**场景**: "How does opencode start and invoke LLM models?"

- 返回 `Entry Points`: `Entry` (instance-store.ts), `Model` type_aliases
- `getConfiguredAgentVariant` 关键函数
- Atlas 无此功能

#### 4.8 codegraph_files (CodeGraph 特有)

**场景**: 项目文件结构

- **1,966 个文件**
- 语言分布: javascript 9, tsx 405, typescript 1517, yaml 35
- 可限制路径范围 (如 `packages/llm/src`)

### TypeScript 总结

| 维度 | 胜出 | 说明 |
|------|------|------|
| 符号搜索 | CodeGraph | Atlas 0.60 置信度导致搜索覆盖不全 |
| 符号详情 | 平手 | 各有侧重 |
| 调用图 | 平手 | 均不完全 |
| 影响分析 | **CodeGraph** | 33 vs 1 节点，差距巨大 |
| 深度追踪 | Atlas 独有 | trace_callers |
| 文件依赖 | Atlas 独有 | import 分析 |
| 任务上下文 | **CodeGraph 独有** | codegraph_context |
| 文件浏览 | **CodeGraph 独有** | codegraph_files 分组 |
| 探索 | **CodeGraph** | 182 符号/48 文件 + verbatim 源码 |

**核心发现**: Atlas 的 TypeScript 置信度仅 0.60，显著影响搜索和分析质量。CodeGraph 在 TS 上表现更好。

---

## 5. Java (apktool)

### 项目概况
| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 152 | 179 |
| 符号数 | 3,019 | 3,186 |
| 边数 | 10,763 | 7,296 |
| 引用数 | 15,385 | - |
| Atlas 语言置信度 | 0.75 | - |

### 工具全面测试

#### 5.1 atlas_search / codegraph_search

**场景**: 搜索 `ApkDecoder`

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 结果 | 2 个: class + constructor | **10 个**: 含 decode, decodeResources, decodeSources 等方法 |
| Score | 1.06 (class) / 1.08 (constructor) | 无评分 |

#### 5.2 atlas_symbol / codegraph_node

**场景**: 查看 `ApkDecoder` 类详情

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 源码 | ✅ **500+ 行完整源码** | ✅ (通过 codegraph_node decode 显示) |
| Callers | 1 callee (class 级别: getName) | **7 个 callers** (含测试) |
| 内容 | 完整类定义，所有方法体 | 仅特定方法 |
| 方法签名 | 含所有方法返回类型 | 含 return 类型 |

**结论**: Atlas 返回整个类的全部源码，CodeGraph 按方法为单位

#### 5.3 atlas_calls / codegraph_callees

**场景**: `ApkDecoder.decode` 的 callees

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| Callees 数量 | **23** | **14** |
| 覆盖 | 全: writeApkInfo, getVersion, rmdir, mkdir, OS 等 | 同类但更少 |
| 调用者 | 0 (class 级别查询) | **7 个** (含测试) |

**结论**: Atlas 在 Java 上 callees 更丰富 (23 vs 14)

#### 5.4 atlas_path / codegraph_trace

**场景**: `ApkDecoder.decode → decodeResources` 路径

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| **路径成功** | ✅ **成功** | ✅ **成功** |
| 路径质量 | "direct", score 0.925 | 2 hops, 完整内联 |
| 路径展示 | hop-by-hop + score | **每个 hop 的函数体都内联展示** |
| 目标调用 | 仅路径 | **目标函数的 callees 也显示** |

**CodeGraph trace 特点**: 路径上的每个函数都显示完整源码，且终点函数的 callees 也一并展示，无需额外查询。

#### 5.5 atlas_impact / codegraph_impact

**场景**: `ApkDecoder.decode` 变更影响

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 影响节点数 | **30** | 30+ |

**结论**: 两者在 Java 上影响分析能力相近

#### 5.6 atlas_trace (callers) — Atlas 特有

**场景**: 反向追踪 `ApkDecoder.decode` (depth=2)

- 触发 CFG 分析
- CodeGraph 无此功能

### Java 总结

| 维度 | 胜出 | 说明 |
|------|------|------|
| 符号搜索 | 平手 | CodeGraph 显示更多方法 |
| 符号详情 | Atlas | 500+ 行完整源码 |
| 调用图 | Atlas | 23 vs 14 callees |
| 路径追踪 | **平手** | 两者均成功追踪 |
| 影响分析 | 平手 | 30 节点相近 |
| 深度追踪 | Atlas 独有 | trace_callers |

---

## 6. C# (shadowsocks-windows)

### 项目概况
| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 90 | 91 |
| 符号数 | 2,493 | 2,612 |
| 边数 | 39,093 | 4,374 |
| 引用数 | 36,247 | - |
| Atlas 语言置信度 | 0.72 | - |

### 工具全面测试

#### 6.1 atlas_search / codegraph_search

**场景**: 搜索 `ShadowsocksController`

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 结果 | 2 个: class + constructor | **10 个**（含 Start, Stop, Reload, SaveConfig 等方法） |
| 命名方式 | `Shadowsocks.Controller.ShadowsocksController` (全命名空间) | `ShadowsocksController` (简单名) |
| Atlas 搜索 | ✅ 找到 (简单名也可搜到) | ✅ 搜索正常 |

**关键发现**: Atlas C# 内部使用全命名空间限定名，但搜索时简单名也有效

#### 6.2 atlas_symbol / codegraph_node

**场景**: 查看 `ShadowsocksController` 详情

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 简单名查询 | ❌ "Symbol not found" | ✅ 返回构造函数源码 |
| **全限定名查询** | ✅ 返回 **300+ 行完整源码** | - |
| 源码完整度 | 完整类定义（所有字段、方法、事件） | 仅构造函数 |

**核心发现**: Atlas C# 需要全限定名 (`Shadowsocks.Controller.ShadowsocksController`)，简单名搜索可用但在 symbol 查询时失败。CodeGraph 使用简单名即可但仅返回构造函数而非完整类。

#### 6.3 atlas_calls / codegraph_callees

**场景**: `ShadowsocksController()` 构造函数的 callees

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| 全限定名查询 | ✅ **3 个 callees**: StartTrafficStatistics, Process, Load | ✅ **3 个 callees** (同类) |
| 简单名查询 | ❌ Symbol not found | ✅ 成功 |
| 额外 | - | 还有 17 个 import callees |

**结论**: 两者在构造函数调用关系上能力相当

#### 6.4 atlas_path / codegraph_trace

**场景**: 构造函数 → `Configuration.Load` 路径

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| **路径成功** | ✅ **成功** | ✅ **成功** |
| 路径质量 | "indirect" (含动态分派) | 直接追踪 |
| 路径长度 | 4 hops | 同 |
| 阻断节点 | 5 个动态分派节点 (GetDefaultServer, CheckConfig 等) | - |

### C# 总结

| 维度 | 胜出 | 说明 |
|------|------|------|
| 符号搜索 | 平手 | Atlas 要求全限定名 |
| 符号详情 | **Atlas** (全限定名) / CodeGraph (简单名) | 取决于使用方式 |
| 调用图 | 平手 | 均 3 个 callees |
| 路径追踪 | 平手 | 均成功 |
| 代码完整度 | Atlas | 全限定名时返回完整类 |

---

## 7. Python (scrapy example)

### 项目概况
| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 11 | 11 |
| 符号数 | 98 | 159 |
| 边数 | 113 | 214 |
| 引用数 | 692 | - |
| Atlas 语言置信度 | 0.72 | - |

### 测试结果

- 项目仅 **11 个文件**，为 scrapy 示例项目而非完整 scrapy 框架
- 搜索 `ScrapySpider` 两个工具均无结果
- 搜索 `Spider` 两个工具均无结果
- CodeGraph: 11 文件, 159 nodes, 214 edges

**结论**: Python 示例项目过小，无法进行有意义的对比测试。需要更大的 Python 代码库（如完整 scrapy 或 django）。

---

## 8. Cangjie (cjvs)

### 项目概况
| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | **24** | **1** |
| 符号数 | **191** | **0** |
| 边数 | **176** | **0** |
| 引用数 | 667 | 0 |
| Atlas 语言置信度 | 0.65 | - |

### 测试结果

| 对比项 | Atlas | CodeGraph |
|--------|-------|-----------|
| **索引能力** | ✅ **完整索引 24 个文件，191 符号** | ❌ **仅索引 1 个 yaml 文件，0 代码符号** |
| `main` 函数 | ✅ 找到 (src/main.cj:14) | ❌ 不支持 Cangjie 语言 |
| 符号搜索 | ✅ 正常运作 | ❌ 不运作 |
| 边数 | 176 条边 | 0 |

**核心发现**: Cangjie 是 Atlas **独有的语言支持**。CodeGraph 完全不支持 Cangjie。Atlas v1.3.1 对 Cangjie 的置信度为 0.65，能进行基本的符号提取和引用分析。

---

## 综合排名

### Atlas 优势领域
| 工具/能力 | 说明 | 支持语言 |
|-----------|------|----------|
| **atlas_path** | 路径追踪，处理歧义强 | C, Java, Rust, C# (Go 接口除外) |
| **atlas_trace (callers)** | 深度调用链追踪 | C, Go, Rust, TS, Java |
| **atlas_calls** | 边类型区分 (calls/references/instantiates) | 所有语言 |
| **atlas_file_dependencies** | 文件级依赖分析 | C (38 includes), TS (13 imports) |
| **atlas_explore** | 边类型精确标注 | 所有语言 |
| **Cangjie 支持** | 唯一支持 Cangjie 的工具 | Cangjie |
| **歧义处理** | score 排序 + scope 过滤 | 所有语言 |

### CodeGraph 优势领域
| 工具/能力 | 说明 | 支持语言 |
|-----------|------|----------|
| **codegraph_context** | 任务导向的上下文汇总 | 所有语言 |
| **codegraph_explore** | 返回 verbatim 源码，免 Read | 所有语言 |
| **codegraph_impact** | 影响分析更全面 | C (390 vs 30), TS (33 vs 1) |
| **codegraph_files** | 文件浏览和统计 | 所有语言 |
| **codegraph_node** (Rust) | 自动列出成员方法 | Rust (35 方法) |
| **搜索便利性** | 简化命名，无需 lifetime/namespace | Rust, C# |

### 各语言置信度对比
| 语言 | Atlas 置信度 | Atlas 优势 | CodeGraph 优势 |
|------|-------------|-----------|----------------|
| **Go** | **0.78** | ✅ 精准符号解析 | ❌ 符号名解析弱 |
| **Java** | 0.75 | ✅ 完整类源码 | 调用者显示更清晰 |
| **C** | 0.73 | ✅ 路径追踪 | ❌ 歧义处理弱 |
| **C#** | 0.72 | ✅ 全限定名完整源码 | 简单名更易用 |
| **Python** | 0.72 | - | - (示例过小) |
| **Rust** | 0.70 | 调用图丰富 | ✅ 简化命名+成员方法展示 |
| **TS** | **0.60** | ❌ 覆盖不全 | ✅ 影响分析强大 |
| **Cangjie** | 0.65 | ✅ **唯一支持** | ❌ 完全不支持 |

*（报告结束）*
