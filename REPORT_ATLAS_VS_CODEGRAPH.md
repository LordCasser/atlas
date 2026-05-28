# Atlas vs CodeGraph MCP 工具行为对比报告

> 报告生成日期: 2026-05-28
> 环境: 每个 example 项目下均执行了 `codegraph init .` 和 `codegraph index`
> Atlas 版本: 1.0.0 (Rust 实现), CodeGraph 版本: 11.10.1 (TypeScript 实现)

---

## 总体概述

本报告对 8 种编程语言的 example 项目进行 Atlas 和 CodeGraph 的 MCP 工具行为对比。Atlas 是 Rust 实现的代码分析引擎，CodeGraph 是 TypeScript 实现的代码分析引擎（基于 tree-sitter）。两者均提供 MCP 协议接口。

### 对比方法论

对每种语言，执行以下维度的对比：
1. **项目索引** — 索引的文件数、节点数、时间
2. **搜索能力** — 符号搜索的精确度和覆盖面
3. **符号详情** — 单个符号的信息丰富度
4. **调用图** — 调用/被调关系追踪
5. **唯一工具** — 各自独有的工具能力
6. **异常情况** — 工具调用失败或行为异常

---

## 1. C 语言 (c_example)

**项目**: curl (HTTP 客户端库) — 一个成熟的 C 项目

### 1.1 索引统计

| 指标 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件数 | 732 | 755 |
| 符号/节点数 | 11,276 | 11,187 |
| 边数 | 51,416 | 25,600 |
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
| 额外符号 | — | `curl_easy_perform_ev`, `curl_easy_send`, `curl_easy_recv`, `Curl_close` |

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

**结论**: 两者在符号详情上基本一致，都能精确追踪 C 函数的调用关系。

#### 1.2.4 调用者/被调者 (callers/callees)

| 方面 | Atlas | CodeGraph |
|------|-------|-----------|
| 结果一致性 | callees: 1 (easy_perform) | callees: 1 (easy_perform @ line 645) |
| 行号差异 | easy_perform @ line 644 | easy_perform @ line 645 |
| API 风格 | 返回结构化 JSON | 返回 markdown 文本 |
| 限制参数 | limit 支持 | limit 支持 |

**行号差异说明**: Atlas 报告 `easy_perform` 在行 644（函数定义），CodeGraph 报告行 645（函数签名后第一行）。这是行计数方式的差异（定义 vs 实现体起点）。

#### 1.2.5 调用图 (callgraph)

| 方面 | Atlas `atlas_callgraph` | CodeGraph `codegraph_impact` |
|------|------------------------|------------------------------|
| 深度 | 支持 depth 参数 | 支持 depth 参数 |
| 方向 | BFS 双向 | 影响分析（双向可达） |
| 结果格式 | 分层 JSON（depth 分组） | 按文件分组的符号列表 |
| 用途 | 调用链追踪 | 变更影响分析 |

#### 1.2.6 上下文工具 (context)

**Atlas `atlas_context`**: 返回 markdown 格式的符号概述，包含源码、调用者示例（含代码片段）、被调者、文件同行符号。调用者部分用文件哈希表示，不够直观，但包含调用行代码示例。

**CodeGraph `codegraph_context`**: 提供任务导向的上下文，搜索 + node + callers + callees 的组合输出。

---

### 1.3 C 语言总结

| 指标 | 胜出方 | 说明 |
|------|--------|------|
| 索引边数 (粒度) | **Atlas** | 51,416 vs 25,600，边粒度更细 |
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
| 文件数 | 待查 (需打开项目) | 90 |
| 符号/节点数 | — | 2,612 |
| 边数 | — | 2,522 |
| 索引时间 | — | 445ms |
| 语言能力 | C#: dataflow_full, confidence 0.72 | — |

### 2.2 工具对比

（待 Atlas 打开项目后更新）
