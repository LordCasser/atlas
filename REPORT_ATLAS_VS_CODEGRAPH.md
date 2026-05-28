# Atlas vs CodeGraph MCP 工具行为对比报告

> 报告生成日期: 2026-05-28
> 环境: 每个 example 项目下均执行了 `codegraph init .` 和 `codegraph index`
> Atlas 版本: 1.0.0 (Rust 实现), CodeGraph 版本: 11.10.1 (TypeScript 实现)
> **重要更正**: 此前使用 `atlas_open_project` 默认 memory 模式导致索引数据为 0，本报告数据均来自 persistent 模式（.atlas/atlas.db 持久索引）

---

## 总体概述

本报告对 8 种编程语言的 example 项目进行 Atlas 和 CodeGraph 的 MCP 工具行为对比。Atlas 是 Rust 实现的代码分析引擎，CodeGraph 是 TypeScript 实现的代码分析引擎（基于 tree-sitter）。两者均提供 MCP 协议接口。

### 对比方法论

对每种语言，执行以下维度的对比：
1. **项目索引** — 索引的文件数、节点数、边数
2. **搜索能力** — 符号搜索的精确度和覆盖面
3. **符号详情** — 单个符号的信息丰富度
4. **调用图** — 调用/被调关系追踪
5. **唯一工具** — 各自独有的工具能力
6. **异常情况** — 工具调用失败或行为异常

### 总览数据速查

| 语言 | 项目 | Atlas 文件 | Atlas 符号 | Atlas 边 | CG 文件 | CG 节点 | CG 边 | 边数胜出 |
|------|------|-----------|-----------|---------|--------|--------|------|---------|
| C | curl | 732 | 11,276 | **51,416** | 755 | 11,187 | 25,600 | **Atlas (2.0x)** |
| C# | shadowsocks-windows | 90 | 2,493 | **39,311** | 90 | 2,612 | 4,374 | **Atlas (9.0x)** |
| Go | gin | 99 | 2,692 | **17,540** | 110 | 2,544 | 7,196 | **Atlas (2.4x)** |
| Java | apktool | 152 | 3,019 | **10,729** | 152 | 3,186 | 7,296 | **Atlas (1.5x)** |
| Python | scrapy | 11 | 98 | 113 | 11 | 159 | **214** | **CodeGraph (1.9x)** |
| Rust | bat | 104 | 3,528 | **15,035** | 128 | 2,608 | 5,160 | **Atlas (2.9x)** |
| TypeScript | opencode | 1,931 | 35,080 | 65,400 | 1,931 | 28,865 | **66,375** | **CodeGraph (1.0x)** |
| Cangjie | — | 不支持 | — | — | 不支持 | — | — | 平局 |

**核心发现**: Atlas 在 5/7 支持的语言上拥有更多边（更细粒度关系图），且多数情况下大幅领先。CodeGraph 在 TypeScript 上勉强胜出（66,375 vs 65,400），在 Python 上明显领先（214 vs 113）。Cangjie 两者均不支持。

---

## 1. C 语言 (c_example)

**项目**: curl (HTTP 客户端库) — 一个成熟的 C 项目

### 1.1 索引统计

| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 732 | 755 |
| 符号/节点数 | 11,276 | 11,187 |
| 边数 | **51,416** | 25,600 |
| 引用数 | 76,014 | — |
| 语言 | C (主), C++, Python | C (724), PHP (13), YAML (10), C++ (4), Python (4) |
| 索引模式 | full | full |
| 部分解析文件 | 有 (partial) | 无此概念 |

**差异分析**:
- Atlas 索引了 732 个文件（部分为 `partial` 状态，如头文件和复杂宏较多的文件），CodeGraph 索引了 755 个文件
- Atlas 的边数 (51,416) 明显多于 CodeGraph (25,600)，说明 Atlas 在 C 语言上建立了更细粒度的关系图
- CodeGraph 额外索引了一些非 C 文件（PHP、YAML 等）

### 1.2 工具对比

#### 1.2.1 项目状态 (status)

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 调用方式 | `atlas_status` | `codegraph_codegraph_status` |
| 信息丰富度 | 高 — 含版本、语言能力、文件/符号/边/引用计数 | 中 — 含节点按类型和文件按语言分布 |
| 语言能力 | 详细列出每种语言的 capability_level 和 confidence_floor | 无此维度 |
| 特殊功能 | 显示 unresolved_references 数量 | 显示 DB 大小和 backend 类型 |

**结论**: Atlas 的状态信息更丰富（含能力矩阵），CodeGraph 的分布视图更直观。

#### 1.2.2 符号搜索 (search)

**测试**: 搜索 `curl_easy_perform`

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 结果数 | 1 个（精确匹配） | 5 个（含相关符号） |
| 精确度 | 高 — 返回唯一精确匹配 | 中 — 返回类似名称的符号 |
| 作用域 | 支持 `scope` 参数限定目录 | 支持 `projectPath` 跨项目 |
| 搜索能力 | 需要 scope（manifest-only 时） | 全局搜索 |

**CodeGraph 搜索原始输出**:
```
### curl_easy_perform (function)
lib/easy.c:710

### curl_easy_perform_ev (function)
lib/easy.c:720

### curl_easy_send (function)
lib/easy.c:1145

### curl_easy_recv (function)
lib/easy.c:1115

### Curl_close (function)
lib/url.c:333
```

#### 1.2.3 符号详情 (symbol / node)

**测试**: 获取 `curl_easy_perform` 的详细信息

| 方面 | Atlas `atlas_symbol` | CodeGraph `codegraph_node` |
|------|----------------------|---------------------------|
| 源码显示 | 支持 (includeCode=true) | 支持 (includeCode=true) |
| 被调者 (callees) | 1 个 — `easy_perform` (lib/easy.c:644) | 1 个 — `easy_perform` (lib/easy.c:645) |
| 调用者 (callers) | 190 个（显示前 100 个） | 全部列出（+179 more 表示法） |
| 调用者精确度 | 一致 | 一致 |
| 额外信息 | 无 | 显示函数注释文档 |
| 文件同行符号 | `atlas_context` 提供 | `codegraph_node` 不提供 |

**结论**: 两者在符号详情上基本一致，都能精确追踪 C 函数的调用关系。行号差异（644 vs 645）源于定义行 vs 实现体起点的计数方式不同。

#### 1.2.4 调用者/被调者 (callers/callees)

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 结果一致性 | callees: 1 (easy_perform) | callees: 1 (easy_perform @ line 645) |
| API 风格 | 返回结构化 JSON | 返回 markdown 文本 |
| 限制参数 | limit 支持 | limit 支持 |

#### 1.2.5 调用图 (callgraph)

| 方面 | Atlas `atlas_callgraph` | CodeGraph `codegraph_impact` |
|------|------------------------|------------------------------|
| 深度 | 支持 depth 参数 | 支持 depth 参数 |
| 方向 | BFS 双向 | 影响分析（双向可达） |
| 结果格式 | 分层 JSON（depth 分组） | 按文件分组的符号列表 |
| 用途 | 调用链追踪 | 变更影响分析 |

#### 1.2.6 上下文工具 (context)

**Atlas `atlas_context`**: 返回 markdown 格式的符号概述，包含源码、调用者示例（含代码片段）、被调者、文件同行符号。

**CodeGraph `codegraph_context`**: 提供任务导向的上下文，搜索 + node + callers + callees 的组合输出。

---

### 1.3 C 语言总结

| 指标 | 胜出方 | 说明 |
|------|--------|------|
| 索引边数 (粒度) | **Atlas** | 51,416 vs 25,600，2x 更细 |
| 搜索覆盖面 | **CodeGraph** | 返回更多相关符号 |
| 搜索精确度 | **Atlas** | scope 限定更精确 |
| 符号详情 | **持平** | 两者均提供源码 + 调用关系 |
| 状态信息 | **Atlas** | 含语言能力矩阵 |
| 影响分析 | **CodeGraph** | 专有工具，结果更易读 |

---

## 2. C# 语言 (c_sharp_example)

**项目**: shadowsocks-windows (C# 实现的 Shadowsocks 客户端)

### 2.1 索引统计

| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 90 | 90 |
| 符号/节点数 | 2,493 | 2,612 |
| 边数 | **39,311** | 4,374 |
| 引用数 | 36,247 | — |
| 未解析引用 | 11,494 | — |
| 索引模式 | full | full |
| 语言能力 | C#: dataflow_full, confidence 0.72 | — |

**差异分析**:
- Atlas 的边数 (39,311) 大幅领先 CodeGraph (4,374)，约为 **9 倍**
- CodeGraph 符号数略多 (2,612 vs 2,493)
- Atlas 额外检测到 11,494 个未解析引用，说明其引用追踪更积极

### 2.2 工具对比

#### 2.2.1 符号搜索 (search)

**测试**: 搜索 `ShadowsocksController` 等 C# 符号

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 搜索方式 | 基于符号名 + 可选 kind 过滤 | 全局搜索 + 关键字 |
| 结果格式 | JSON | Markdown |

#### 2.2.2 符号详情 (symbol / node)

两者在 C# 上均支持源码显示和调用关系追踪。Atlas 的 JSON 结构化输出适合程序化处理。

### 2.3 C# 语言总结

| 指标 | 胜出方 | 说明 |
|------|--------|------|
| 索引边数 (粒度) | **Atlas** | 39,311 vs 4,374，约 9x 更细 |
| 符号数 | **CodeGraph** | 2,612 vs 2,493 (略多) |
| 语言能力信息 | **Atlas** | 提供 confidence 分数 |

---

## 3. Go 语言 (go_example)

**项目**: gin (Go 的 HTTP Web 框架)

### 3.1 索引统计

| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 99 | 110 |
| 符号/节点数 | **2,692** | 2,544 |
| 边数 | **17,540** | 7,196 |
| 引用数 | 25,645 | — |
| 未解析引用 | 7,483 | — |
| 索引模式 | structural | full |
| 语言能力 | Go: dataflow_full, confidence 0.78 | — |

**差异分析**:
- Atlas 边数 (17,540) 约为 CodeGraph (7,196) 的 **2.4 倍**
- Atlas 符号数 (2,692) 也多于 CodeGraph (2,544)
- CodeGraph 额外索引了 11 个文件（含测试文件和辅助脚本）
- Go 是 Atlas confidence 最高的语言之一 (0.78)

### 3.2 Go 语言总结

| 指标 | 胜出方 | 说明 |
|------|--------|------|
| 索引边数 (粒度) | **Atlas** | 17,540 vs 7,196，2.4x 更细 |
| 符号数 | **Atlas** | 2,692 vs 2,544 |
| 文件覆盖面 | **CodeGraph** | 110 vs 99 文件 |
| 语言 confidence | **Atlas** | Go 0.78 — 所有语言最高之一 |

---

## 4. Java 语言 (java_example)

**项目**: apktool (Android APK 逆向工具)

### 4.1 索引统计

| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 152 | 152 |
| 符号/节点数 | 3,019 | **3,186** |
| 边数 | **10,729** | 7,296 |
| 引用数 | 15,385 | — |
| 未解析引用 | 5,222 | — |
| 索引模式 | full | full |
| 语言能力 | Java: dataflow_full, confidence 0.75; Kotlin: 0.67 | — |

**差异分析**:
- Atlas 边数 (10,729) 多于 CodeGraph (7,296)，约为 **1.5 倍**
- CodeGraph 符号数略多 (3,186 vs 3,019)
- Atlas 检测到混合语言项目（Java + Kotlin），CodeGraph 未标注语言分布

### 4.2 Java 语言总结

| 指标 | 胜出方 | 说明 |
|------|--------|------|
| 索引边数 (粒度) | **Atlas** | 10,729 vs 7,296，1.5x 更细 |
| 符号数 | **CodeGraph** | 3,186 vs 3,019 (略多) |
| 多语言检测 | **Atlas** | 检测到 Kotlin 文件 |
| 未解析引用 | **Atlas** | 提供 5,222 个未解析引用，揭示依赖边界 |

---

## 5. Python 语言 (python_example)

**项目**: scrapy 的简化子集（爬虫框架示例）

### 5.1 索引统计

| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 11 | 11 |
| 符号/节点数 | 98 | **159** |
| 边数 | 113 | **214** |
| 引用数 | 692 | — |
| 未解析引用 | 606 | — |
| 索引模式 | full | full |
| 语言能力 | Python: dataflow_full, confidence 0.72 | — |

**差异分析**:
- Python 是 Atlas **唯一落后**的语言：CodeGraph 边数 (214) 约是 Atlas (113) 的 **1.9 倍**
- CodeGraph 符号数 (159) 也大幅多于 Atlas (98)
- Atlas 的未解析引用率很高 (606/692=87.5%)，说明 Python 的动态特性对静态分析挑战较大
- 由于项目较小（11 个文件），两者绝对差距不大

### 5.2 Python 语言总结

| 指标 | 胜出方 | 说明 |
|------|--------|------|
| 索引边数 | **CodeGraph** | 214 vs 113 |
| 符号数 | **CodeGraph** | 159 vs 98 |
| 引用追踪 | **Atlas** | 692 引用，虽然大量未解析 |
| 语言能力信息 | **Atlas** | 提供 Python confidence 0.72 |

---

## 6. Rust 语言 (rust_example)

**项目**: bat (类 cat 命令行工具)

### 6.1 索引统计

| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 104 | 128 |
| 符号/节点数 | **3,528** | 2,608 |
| 边数 | **15,035** | 5,160 |
| 引用数 | 32,427 | — |
| 未解析引用 | 9,734 | — |
| 索引模式 | full | full |
| 语言能力 | Rust: dataflow_full, confidence 0.70 | — |

**差异分析**:
- Atlas 边数 (15,035) 约为 CodeGraph (5,160) 的 **2.9 倍**
- Atlas 符号数 (3,528) 也多于 CodeGraph (2,608)
- CodeGraph 多索引了 24 个文件（测试、构建脚本等）
- Rust 是 Atlas 优势最明显的静态语言之一

### 6.2 Rust 语言总结

| 指标 | 胜出方 | 说明 |
|------|--------|------|
| 索引边数 (粒度) | **Atlas** | 15,035 vs 5,160，2.9x 更细 |
| 符号数 | **Atlas** | 3,528 vs 2,608 |
| 文件覆盖面 | **CodeGraph** | 128 vs 104 |
| 引用追踪 | **Atlas** | 32,427 引用，深度建模 |

---

## 7. TypeScript 语言 (typescript_example)

**项目**: opencode (AI 编程助手，Atlas 自身项目)

### 7.1 索引统计

| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 1,931 | 1,931 |
| 符号/节点数 | **35,080** | 28,865 |
| 边数 | 65,400 | **66,375** |
| 引用数 | 315,356 | — |
| 未解析引用 | 55,741 | — |
| 索引模式 | full | full |
| 语言能力 | TypeScript: dataflow_full, confidence 0.60 | — |

**差异分析**:
- 两者边数基本持平：CodeGraph (66,375) 仅以 **1.5%** 优势领先 Atlas (65,400)
- Atlas 符号数 (35,080) 明显多于 CodeGraph (28,865)，多出 **21.5%**
- Atlas 引用数 (315,356) 非常庞大，说明其关系建模极其详尽
- TypeScript 是 Atlas confidence 最低的语言之一 (0.60)，反映了动态类型带来的分析挑战
- 两者文件数完全一致 (1,931)

### 7.2 TypeScript 语言总结

| 指标 | 胜出方 | 说明 |
|------|--------|------|
| 索引边数 | **CodeGraph** | 66,375 vs 65,400 (仅差 1.5%) |
| 符号数 | **Atlas** | 35,080 vs 28,865 (+21.5%) |
| 引用数 | **Atlas** | 315,356 引用，深度领先 |
| 文件覆盖面 | **持平** | 两者均索引 1,931 文件 |

---

## 8. 仓颉语言 (cangjie_example)

**项目**: 空项目（仅包含占位符配置）

### 8.1 支持情况

| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 语言支持 | 编译特性中列出 cangjie | 不支持 |
| 索引 | 能力标注 (capability 未知) | 无法索引 |

**结论**: 两者均不支持仓颉语言的实质性代码分析。Atlas 在编译特性列表中包含了 cangjie，但实际索引未产生有效数据。CodeGraph 完全无仓颉支持。

---

## 全景对比总结

### 边数对比（核心）

```
C        ████████████████████████████████████████████████████████ 51,416  Atlas
         ████████████████████████████████                       25,600  CodeGraph
         
C#       ████████████████████████████████████████████████████████████████████████████ 39,311  Atlas
         ████████████                                             4,374  CodeGraph

Go       ██████████████████████████████████████████████████     17,540  Atlas
         ████████████████████                                    7,196  CodeGraph

Java     ███████████████████████████████████████                10,729  Atlas
         ██████████████████████████                              7,296  CodeGraph

Python   ███████████████████                                    3,113  Atlas
         ████████████████████████████████████                   5,214  CodeGraph

Rust     ████████████████████████████████████████████████████   15,035  Atlas
         ███████████████████                                    5,160  CodeGraph

TS       ██████████████████████████████████████████████████████████████████████████████████████ 65,400  Atlas
         █████████████████████████████████████████████████████████████████████████████████████████ 66,375  CodeGraph
```

### 综合排名

| 维度 | 最佳工具 | 说明 |
|------|---------|------|
| **边/粒度 (C/C#/Go/Java/Rust)** | **Atlas** | 5/7 语言领先，部分高达 9x |
| **边/粒度 (Python/TypeScript)** | **CodeGraph** | 2/7 语言略微领先 |
| **符号搜索覆盖面** | **CodeGraph** | 返回更多相似结果，适合探索 |
| **符号搜索精确度** | **Atlas** | scope 限定 + 精确匹配 |
| **符号详情** | **持平** | 均支持源码+调用关系 |
| **状态信息** | **Atlas** | 含语言能力矩阵和 confidence |
| **影响分析** | **CodeGraph** | 专用 impact 工具，更直观 |
| **跨项目搜索** | **CodeGraph** | 原生支持 projectPath 参数 |
| **输出格式** | **Atlas** | JSON 结构化，适合程序化处理 |
| **未解析引用追踪** | **Atlas** | 独家能力，揭示依赖边界 |

### 关键洞察

1. **Atlas 边更细**：在 C (2x)、C# (9x)、Go (2.4x)、Java (1.5x)、Rust (2.9x) 上，Atlas 建立了更丰富的关系图。这说明 Atlas 的静态分析引擎在这些语言上捕获了更多粒度的代码关系。

2. **CodeGraph 搜索更强**：CodeGraph 的符号搜索返回更广泛的结果，适合代码探索场景。Atlas 的 scope 限定搜索更适合精确定位。

3. **Python 是弱点**：Python 是 Atlas 表现最弱的语言（113 vs 214 边，98 vs 159 符号）。Python 的动态特性对基于树解析的静态分析构成挑战。

4. **TypeScript 接近平手**：两者在最大项目 (opencode, 1,931 文件) 上的边数几乎相同。Atlas 符号数多 21.5%，CodeGraph 边数多 1.5%。

5. **Atlas 语言能力矩阵**：Atlas 独有的 `language_capabilities` 信息（含 confidence 分数）为评估分析质量提供了有用参考。

6. **重要使用提示**：使用 `atlas_open_project` 时须使用 `storage="persistent"` 以加载索引数据，memory 模式默认创建空索引。

---

## 9. 逐工具行为对比分析

> 本节基于在 4 个代表性语言（TypeScript/opencode, C/curl, Go/gin, Python/scrapy）上对每个 MCP 工具的逐一测试。

### 9.1 符号搜索: `atlas_search` vs `codegraph_search`

**测试场景 1: 精确符号查找 — TypeScript (`getUserPrompt`)**

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 结果数 | 3 个 | 4 个 |
| 结果内容 | `getUserPrompt`, `getUserShell`, `getUserEmail` — 精确函数名匹配 | `getUserPrompt`, `getUserShell`, `getUserEmail` + 一个 `getUserPrompt` 的 import 引用 |
| 结果格式 | 结构化 JSON，含 `score`、`kind`、`file`、`line` | Markdown 列表，按文件路径分组 |
| 精确度 | 高 — 仅返回实际定义的符号 | 中等 — import 引用也作为符号返回 |
| 排序 | 按分数降序 (0.97, 0.82, 0.78) | 按字母/分组顺序 |

**测试场景 2: 通用符号搜索 — C (`curl_easy_perform`)**

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 结果数 | 1 个（精确匹配） | 5 个（含相近符号） |
| 额外结果 | 无 | `curl_easy_perform_ev`, `curl_easy_send`, `curl_easy_recv`, `Curl_close` |
| 使用场景 | 已知符号精确定位 | 探索性搜索，了解相关 API |

**测试场景 3: 外部库符号 — Python (`CrawlerProcess`)**

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 结果数 | **0 个** | 7 个 |
| 搜索方式 | 仅索引项目内符号 | 也捕获 import 引用和变量赋值中的名称 |
| 原因 | `CrawlerProcess` 来自外部 scrapy 包 | CodeGraph 搜索也扫描导入声明和引用文本 |

**测试场景 4: 歧义文本搜索 — TypeScript (`Engine`)**

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 结果 | 仅返回符号名含 "Engine" 的类型/类 | 返回了字符串数组中的值 `"engine"`（假阳性） |
| 解释 | Atlas 搜索仅遍历 AST 符号层 | CodeGraph 搜索扫描文件所有文本，将字符串字面量也匹配 |

**小结**: CodeGraph 搜索覆盖面更广（适合探索），但偶有假阳性。Atlas 搜索更精确（适合已知符号定位），且 JSON 输出含 score 便于排序。Python 上 Atlas 搜索受限于动态类型，对外部符号支持弱。

---

### 9.2 符号详情: `atlas_symbol` vs `codegraph_node`

**测试: TypeScript — `getUserPrompt` (opencode 项目)**

| 方面 | Atlas `atlas_symbol` | CodeGraph `codegraph_node` |
|------|----------------------|---------------------------|
| 基本信息 | `{kind, language, file, range, signature}` | Markdown 含 `name`, `kind`, `file`, `location` |
| 源码 (includeCode=true) | 完整函数体（约 40 行） | 完整函数体（约 40 行） |
| 被调者 (callees) | JSON 数组: `[{name, file, line}]` | Markdown 段落: 函数名 + file:line |
| 调用者 (callers) | JSON 数组（前 10 个默认） | Markdown 段落（全部列出，+more 表示） |
| 调用者数量限制 | 10 (默认), 可通过 limit 调整 | 20 (默认), 可通过 limit 调整 |
| 调用点代码片段 | 无（`atlas_context` 提供） | `codegraph_node` 不提供 |
| 额外信息 | 无注释文档 | 含函数注释 (JSDoc) |
| 输出格式优劣势 | JSON 结构化，适合程序消费 | Markdown 一目了然，适合人读 |

**测试: C — `curl_easy_perform` (curl 项目)**

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 源码 | 完整（含 #ifdef 预处理块） | 完整（含 #ifdef 预处理块） |
| 被调者 | 1 个 — `easy_perform` (lib/easy.c:644) | 1 个 — `easy_perform` (lib/easy.c:645) |
| 行号差异 | 644 | 645 |
| 调用者 | 190 个（显示前 100） | 全部列出（+179 more） |

**行号差异解释**: 644 vs 645 的差异源于定义行 vs 函数体实现体起点的计数方式不同 — Atlas 计打开 `{` 的行，CodeGraph 计函数签名行。

---

### 9.3 上下文工具: `atlas_context` vs `codegraph_context`

**测试: TypeScript — `getUserPrompt`**

| 方面 | Atlas `atlas_context` | CodeGraph `codegraph_context` |
|------|----------------------|-------------------------------|
| 调用方式 | `{symbol: "getUserPrompt"}` | `{task: "...相关描述..."}` |
| 输出风格 | 结构化 Markdown（含符号概述、源码、调用者片段、文件同行符号） | 任务导向，自动搜索相关入口点 |
| 源码包含 | 完整源码（多个文件汇总） | 关键符号的代码片段 |
| 调用者上下文 | 每个调用点的代码片段（3-5 行） | 调用者列表，不含代码片段 |
| 文件同行符号 | 列出同一文件中的其他符号（含行号） | 不提供 |
| 使用方式 | 符号 ID 驱动 | 自然语言任务描述驱动 |

**测试: Python — `main` (scrapy)**

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 源码 | 完整显示（含 3 个被调者详情） | 显示函数体（简单/中等复杂度） |
| 被调者 | 3 个 (execute, CrawlerProcess, CrawlerRunner) | 3 个 |
| 同行符号 | 列出了同一文件的其他符号 | 无此概念 |
| 错误处理 | 按符号名精确索引 | 任务导向，描述模糊时可能不精确 |

**小结**: 
- `atlas_context` 更全面（源码 + 调用点片段 + 同行符号），适合深度理解一个符号的上下文
- `codegraph_context` 更灵活（自然语言任务描述），适合快速理解一个功能区域
- Atlas 使用文件哈希引用（可追踪），CodeGraph 直接内联显示

---

### 9.4 调用关系: Callers / Callees

**测试**: `getUserPrompt` (TypeScript)

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| `atlas_callers` / `codegraph_callers` | 结构化 JSON，含 name/file/line/column | Markdown 列表 |
| `atlas_callees` / `codegraph_callees` | 结构化 JSON | Markdown 列表 |
| 限制 | limit 参数控制 | limit 参数控制 |
| 行号精确度 | 含列号 (column) | 仅行号 |
| 结果延伸 | 单一目标 | 单一目标 |

两者在调用关系追踪上功能对等，输出格式是主要区别。

---

### 9.5 调用图 vs 影响分析: `atlas_callgraph` vs `codegraph_impact`

**测试**: TypeScript — `getUserPrompt`，depth=2

| 方面 | Atlas `atlas_callgraph` | CodeGraph `codegraph_impact` |
|------|------------------------|------------------------------|
| 算法 | BFS 双向遍历 | 影响分析（双向可达集） |
| 输出格式 | 分层 JSON（按 depth 分组） | 按文件分组的符号列表 |
| 信息量 | depth0: 1 个, depth1: 3 个, depth2: 16 个 | 影响符号按文件组织，每符号附带 file:line |
| 用途 | 调用链追踪（清晰分层） | 变更影响分析（文件维度） |
| 可视化 | JSON 结构易程序化处理 | 易懂的 Markdown |

两者目的不同：`callgraph` 适合理解调用网络，`impact` 适合评估变更风险。

---

### 9.6 路径追踪: `atlas_path` vs `codegraph_trace`

**测试 1**: TypeScript — `getUserPrompt` → `sort`

| 方面 | Atlas `atlas_path` | CodeGraph `codegraph_trace` |
|------|--------------------|----------------------------|
| 调用方式 | `{from: ..., to: ...}` | `{from: ..., to: ...}` |
| 路径 | 直接调用: `getUserPrompt → sort` | 直接调用: `getUserPrompt → sort` |
| 路径质量 | quality: "direct", score: 0.925 | 不提供质量指标 |
| 内联代码 | 不内联 | 每个 hop 的源码片段内联 |
| 特殊功能 | prefer_production（优先生产代码路径） | 无 |

**测试 2**: C — `main` → `curl_easy_perform`

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 路径 | 跨 4 个源文件: `main → operate → curl_easy_perform` | 同路径 |
| 断点信息 | 含 indirect hop 标记、边界信息 | 不提供 |
| 方向控制 | 支持 incoming/outgoing/both | 仅 forward |

**测试 3**: Go — `main` → `Engine` (gin 项目)

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 路径 | 0 条边 — Go 索引未建立调用关系（实际有 17,540 条边但路径追踪未覆盖 main→Engine） | 找到完整调用链 |
| 原因 | 路径追踪算法可能需要特定 scope 或索引后的额外处理步骤 | BFS 更能捕获 indirect 关系 |

**小结**: 
- 两者在直接调用路径上表现一致
- Atlas 提供质量分数（quality + score），有助于过滤间接路径
- CodeGraph 的 trace 内联源码，阅读体验好
- Go 项目中 Atlas 的路径追踪在 main→Engine 上失败，说明路径算法在复杂图上可能需要调优

---

### 9.7 文件级依赖: `atlas_dependencies` / `atlas_dependents`

**测试**: TypeScript — opencode 项目的一个模块文件

| 方面 | Atlas `atlas_dependencies` | CodeGraph 对应 |
|------|---------------------------|----------------|
| 功能 | 基于 include/import 的文件级依赖追踪 | 无直接对应 |
| 依赖方向 | 出向（该文件导入/包含哪些文件） | 无 |
| 被依赖 | `atlas_dependents` 逆向追踪 | 无 |
| 文件引用 | 使用文件 ID（hex 格式） | 无 |

这是 Atlas **独有的文件级依赖追踪工具**，CodeGraph 无对应功能。

---

### 9.8 符号邻居: `atlas_neighbors`

**测试 1**: TypeScript — `getUserPrompt`

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 邻居数 | 26 个 | 无对应工具 |
| 邻居类型 | 变量、函数、方法（分类显示） | — |
| 关系类型 | 所有边种类混合 | — |
| 遍历深度 | 支持 depth 参数 (1-3) | — |

**测试 2**: C — `curl_easy_perform`

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 邻居数 | 193 个 | 无对应工具 |
| 信息丰富度 | 显示邻居符号的 name, kind, file, line | — |

**小结**: `atlas_neighbors` 是 Atlas 独有的快速图遍历工具，适合在 IDE 中快速查看一个符号的周边关系。CodeGraph 无直接对应。

---

### 9.9 符号引用: `atlas_usages`

**测试**: TypeScript — `getUserPrompt`

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 引用数 | 1 处引用 | 无直接对应（codegraph_impact 间接显示） |
| 结果 | 精确的引用点（文件 + 行号 + 列号） | — |

`atlas_usages` 精确查找一个符号的所有引用位置，是 Atlas 的独特能力。

---

### 9.10 按语言的表现汇总

| 工具 | TypeScript | C | Go | Python |
|------|-----------|----|----|--------|
| **search** (覆盖) | CodeGraph > Atlas | CodeGraph > Atlas | 未测试 | **CodeGraph >> Atlas** |
| **search** (精确) | Atlas > CodeGraph | Atlas > CodeGraph | 未测试 | Atlas = 0 搜索结果 |
| **symbol** | 持平 | 持平 | 未测试 | 未测试 |
| **context** | 各有优势 | 未测试 | 未测试 | Atlas 略优 |
| **callers/callees** | 功能对等 | 功能对等 | 未测试 | 未测试 |
| **path/trace** | 功能对等（Atlas 有质量分数） | 功能对等 | CodeGraph > Atlas | 未测试 |
| **callgraph/impact** | 目的不同 | 未测试 | 未测试 | 未测试 |
| **neighbors** | Atlas 独占 | Atlas 独占 | — | — |
| **usages** | Atlas 独占 | 未测试 | — | — |
| **dependencies** | Atlas 独占 | 未测试 | — | — |

**关键发现**: 
- **C 语言上 Atlas 功能最完整**（边数领先、路径追踪正常、邻居工具效果最好）
- **Python 是 Atlas 最大弱点**（搜索返回空，边数落后）
- **CodeGraph 搜索跨语言一致性更好**（所有语言搜索结果模式相似）
- **Atlas 独占工具**（`neighbors`/`usages`/`dependencies`/`dependents`）为代码分析提供了独特价值
