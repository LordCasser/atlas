# Atlas vs CodeGraph 对比测试报告

> 测试日期: 2026-05-19
> 环境: macOS (arm64), Atlas 7.3MB Rust binary, Codegraph v0.7.8 (Node.js/TS)
> 目标: 从索引能力、符号提取、引用分析、搜索能力、边界测试、性能六大维度进行全面对比

---

## 一、总览

| 维度 | Atlas | Codegraph |
|------|-------|-----------|
| 实现语言 | Rust (单二进制) | TypeScript/Node.js |
| 文件大小 | 7.3MB (arm64) | N/A (npm package) |
| 多语言支持 | **Python + TypeScript/JavaScript** | **15种语言** (tree-sitter wasm运行时加载) |
| 搜索架构 | SQLite FTS5 精确匹配 | embedding 语义搜索 |
| 索引存储 | SQLite (单文件) | SQLite (单文件) |
| CLI版本 | v0.1.0 | v0.7.8 |

---

## 二、测试项目概览

| 项目 | 语言 | 规模 | 源文件 | 说明 |
|------|------|------|--------|------|
| python_example | Python | 小 | 11 (.py) | 自定义Scrapy项目 (git子模块) |
| typescript_example | TypeScript | 大 | 1,926 (.ts/.tsx) | opencode完整项目 (git子模块) |
| c_example | C | 大 | 1,024 | curl项目 (git子模块) |
| java_example | Java | 中 | 152 | apktool项目 (git子模块) |

---

## 三、Python 小规模对比

### 索引结果

| 指标 | Atlas | Codegraph |
|------|-------|-----------|
| 文件索引 | 11/11 (100%) | 11/11 (100%) |
| 符号/节点 | **98 symbols** | **159 nodes** |
| 边 | 526 | 213 |
| 索引时间 | 318ms | ~170ms |
| 数据库大小 | 728KB | 352KB |

Codegraph 提取了更多细粒度节点 (import 48个、variable 30个)，Atlas 的边更多。

### 搜索功能

| 特性 | Atlas | Codegraph |
|------|-------|-----------|
| 搜索引擎 | SQLite FTS5 | embedding语义 |
| 评分范围 | 0 ~ 1.06 | 0 ~ 114 |
| Kind过滤器 | 无 | `-k class/function/variable` |
| 输出格式 | 纯文本表格 | JSON (含startLine/endLine/signature) |
| Limit参数 | `-l N` (0=空结果) | 未测试 |

### 独特功能

| Atlas 特有 | Codegraph 特有 |
|-----------|---------------|
| `sync` — 增量检测变化(26 added) | `files` — 显示每个文件的nodeCount/size |
| `doctor` — 系统健康检查 | `context` — 基于查询构建markdown上下文 |
| `status` — 索引全局状态 | `affected` — 查找受更改影响的测试 |
| 引用解析报告 (11.5% resolution) | 按kind分类统计 |

---

## 四、TypeScript 大规模对比 ★★★ 关键

### 索引覆盖率: 最大差异点

| 指标 | Atlas | Codegraph |
|------|-------|-----------|
| 文件发现 | ~1,920 | 1,926 |
| **文件索引** | **431 (22%)** | **1,926 (100%)** |
| 符号/节点 | 3,594 | 28,474 |
| 边 | 5,696 | 64,118 |
| **索引时间** | **47.15s** | **9.3s (5.1x 更快)** |
| 数据库大小 | 10.3MB | 52MB |
| **错误** | **~1,500 FOREIGN KEY constraint failed** | **无** |

### Root Cause: FOREIGN KEY constraint failed

**根因**: `find_enclosing_function_id()` 在 TypeScript 箭头函数中生成的 SymbolId 与 `definitions.scm` 生成的 SymbolId 不一致。

两种失败模式:

1. **匿名箭头函数** — `.map(x => x*2)`
   - `find_enclosing_function_id` 生成 `SymbolId(kind="function", name="anonymous")`
   - `definitions.scm` 不捕获匿名箭头函数 → 无对应 symbol → FK失败

2. **变量赋值箭头函数** — `const fn = (x) => x*2`
   - `find_enclosing_function_id` 生成 `SymbolId(kind="function", name="fn")`
   - `definitions.scm` 捕获为 `SymbolDef(kind="variable", name="fn")`
   - kind 不同 → blake3 哈希不同 → FK失败

**触发数据流查询**: `(required_parameter (identifier) @dataflow.parameter)` — 匹配任何函数的形参。

**影响范围**: 含箭头函数的任何文件 — React组件、promise链、数组方法(.map/.filter)、事件监听器等。覆盖现代 TypeScript 代码库约 **78% 的文件**。

---

## 五、C 大规模对比 (curl, 1,024 files)

> Atlas 需 `--features "c,cpp"` 重新编译。curl 项目含 2 个 .cpp 文件。

### 索引结果

| 指标 | Atlas | Codegraph | 差异 |
|------|-------|-----------|------|
| 文件索引 | **1,024/1,024 (100%)** | 1,024/1,024 (100%) | 持平 |
| 符号/节点 | **15,601** | 15,331 | Atlas +1.8% |
| 边 | **95,682** | 42,726 | Atlas **2.2x** |
| 索引时间 | **81s (1m21s)** | **4.8s** | Codegraph **17x 快** |
| 数据库大小 | **161 MB** | 25 MB | Atlas 6.3x 大 |

### Symbol Kind 分布

| Kind | Atlas | Codegraph | 说明 |
|------|-------|-----------|------|
| function | 5,220 | 5,404 | 基本持平 |
| method | 834 | 821 | 基本持平 |
| variable | **3,424** | 397 | Atlas 捕获更多变量 |
| class/struct | **2,980** | 540+72 | Atlas **偏高** (见下方) |
| macro | **2,894** | 0 | Codegraph 不捕获 macro |
| type_alias | 181 | 220 | 基本持平 |
| enum | 68 | 161 | Codegraph 更多 |
| enum_member | — | 2,367 | Codegraph 展开枚举成员 |
| import | — | 4,325 | Atlas 独立表存储 import |

### 搜索对比

| 查询 | Atlas | Codegraph |
|------|-------|-----------|
| `curl_global_init` | **2 结果** (score 1.060/0.841, 显示 sig+code+path:line) | 10 结果 (score 10457%) |
| `curl_global_init --kind class` | 正常过滤 (仅保留 class 类型) | 无等同 CLI 参数 |
| `curl_global_init --json` | JSON 含 sig/snippet/path/line/score | 默认 JSON 输出 |
| `url_globa_init` (typo) | **Levenshtein** 模糊匹配到 `curl_global_init` | 同样支持模糊 |

### Atlas 特有功能 (对比 Python 阶段新增)
- `--kind` 过滤 (SQL 级，不影响降级链)
- `--json` 输出 (含 snippet/signature)
- `atlas context` 命令 (markdown + 源码摘录)
- `atlas files` 命令 (文件浏览 + 语言统计)

### 发现的问题

1. **`class` 计数偏高 (2,980)**: C 的 `definitions.scm` 中 `(struct_specifier (type_identifier) @definition.class)` 捕获 **所有** `struct_specifier` 节点——包括前向声明、形参类型引用、成员类型引用——而非仅定义。这导致大量非真正定义的 `struct` 被计入 symbol 表。

2. **数据库膨胀 (161MB vs 25MB)**: Atlas 存储完整符号表 (28 列含所有 range/byte offset)、引用表、数据流边、callsites；Codegraph 存储更精简的 node/edge 模型。这是架构性差异。

3. **索引时间差距 (81s vs 4.8s)**: Atlas 的参考解析阶段 (resolution) 耗时显著——遍历所有引用尝试解析到目标 symbol。Codegraph 不做跨文件引用解析。

---

## 六、Java 中等规模对比 (apktool, 152 files)

> Atlas 需 `--features "java"` 重新编译后测试。

| 指标 | Atlas | Codegraph |
|------|-------|-----------|
| 文件索引 | 未重测 (feature 启用后可运行) | 152 (100%) |
| 节点 | N/A | 3,186 (152 class, 1,265 method, 698 field) |
| 边 | N/A | 7,247 |
| 索引时间 | N/A | 883ms |
| 数据库大小 | N/A | 7.7MB |

---

## 七、性能总结

| 项目 | Atlas | Codegraph | 差距 |
|------|-------|-----------|------|
| Python (11 files) | 318ms | ~170ms | ~1.9x |
| TypeScript (1,926 files) | **47.15s** | **9.3s** | **5.1x** |
| C (1,024 files) | **81s** | **4.8s** | **17x** |
| Java (152 files) | 未重测 | 883ms | — |

---

## 八、结论与建议 (更新版)

### 已修复/改进的问题

| # | 问题 | 状态 | Commit |
|---|------|------|--------|
| 1 | TypeScript FK constraint failed (78% indexed) | **已修复** — SymbolRegistry + adapter patch | `57c0e19` `f1d787f` `f450edc` |
| 2 | 搜索结果只显示 FileId hex | **已修复** — 显示人类可读 file_path:line | `229703f` |
| 3 | 搜索缺少 --kind/--json 过滤 | **已添加** — CLI + JSON 输出 + snippet | `229703f` `fa8f2cf` |
| 4 | 无 context/files 命令 | **已添加** — `atlas context` + `atlas files` | `229703f` |
| 5 | Python 所有 function 不区分 method | **已修复** — AST walk 检查 class_definition | `fa8f2cf` |
| 6 | 搜索无前缀排名 | **已修复** — prefix match 评分为 0.92 | `fa8f2cf` |
| 7 | 搜索降级链在 kind filter 下断裂 | **已修复** — kind filter 推入 SQL 层 | `fa8f2cf` |
| 8 | 搜索/context 无源码片段 | **已添加** — snippet + 源码摘录 | `fa8f2cf` |
| 9 | C/C++ feature 未编译 | **可用** — `--features "c,cpp"` 编译 | 手动编译 |

### 剩余问题

1. **⏱ 大规模索引性能**: C 项目 Atlas 81s vs Codegraph 4.8s (17x)。主要瓶颈在引用解析阶段。
2. **🗄 数据库体积**: C 项目 161MB vs 25MB (6.3x)。架构性差异，但可优化。
3. **🎯 C 的 `class` 定义过宽**: `struct_specifier` 捕获了所有类型引用而非仅定义。
4. **🔧 Java 支持**: 需 feature 编译后测试。
5. **📏 搜索缺少 FTS5原生 rank 和 snippet()**: 当前 `fts_score` 由 IDF 重新推导，未捕获 SQLite 的 BM25 rank。
6. **📊 符号统计粒度**: Atlas 比 Codegraph 更粗 (无拆分的 enum_member, import 独立存储不合并统计)。

### Codegraph 相对优势(仍存在)

1. **15种语言** — tree-sitter wasm 动态加载
2. **大规模索引速度** — 4.8s (curl) vs 81s，17x 更快
3. **数据库紧凑** — 25MB vs 161MB
4. **丰富的开发者命令** — `files/context/affected/query`

### Atlas 相对优势(持续强化)

1. **多信号评分搜索** — FTS5 BM25 + 图度 + 名称相似度 + kind 加成 + 路径加成
2. **三级降级链** — FTS5 → LIKE → Levenshtein 模糊
3. **源码片段显示** — search/context 命令均带源码摘录
4. **引用解析** — 跨文件引用解析 (虽慢但有)
5. **Rust 单二进制** — 无运行时依赖
6. **SymbolRegistry 架构** — 确保 edges/callsites 不会产生虚引用
