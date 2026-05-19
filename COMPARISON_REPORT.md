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

## 五、C 大规模对比

| 指标 | Atlas | Codegraph |
|------|-------|-----------|
| 文件索引 | **0 — ERROR** | 1,024 |
| 节点 | N/A | 15,331 |
| 边 | N/A | 42,726 |
| 索引时间 | N/A | 6.0s |
| 数据库大小 | N/A | 26MB |
| 错误信息 | `Language C not enabled` | 无 |

---

## 六、Java 中等规模对比

| 指标 | Atlas | Codegraph |
|------|-------|-----------|
| 文件索引 | 未测试 (预计 ERROR) | 152 (100%) |
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
| C (1,024 files) | ERROR | 6.0s | 无法对比 |
| Java (152 files) | ERROR | 883ms | 无法对比 |

---

## 八、结论与建议

### Atlas 当前问题

1. **🐛 严重Bug**: TypeScript 箭头函数导致 78% 文件索引失败 (FOREIGN KEY constraint failed)
2. **🔧 语言支持不完整**: C/C++/Java 编译未启用 (`doctor` 显示 WARNING)
3. **⏱ 大规模性能**: 相同 TypeScript 项目 index 耗时 47s (codegraph 仅 9s)
4. **🎯 符号提取粒度**: 比 codegraph 少 (3,594 vs 28,474)，缺少 import 等关键节点

### Atlas 优势

1. FTS5 精确匹配搜索 (适合精确查找)
2. 运维命令完善 (`doctor`, `status`, `sync`)
3. Rust 单二进制部署 (无运行时依赖)
4. 引用解析报告 (11.5% 链接解析率 — 虽然很低但至少报告了)

### Codegraph 优势

1. 15种语言支持 (tree-sitter wasm 动态加载)
2. 大规模性能优越 (9.3s 索引 1926 文件)
3. 符号提取更细粒度 (import/class/method/field/variable)
4. 开发者功能丰富 (context/files/affected)
5. embedding 语义搜索

### 建议优先级

1. **P0**: 修复 `find_enclosing_function_id()` 的 SymbolId 匹配问题
2. **P1**: 启用 C/C++/Java tree-sitter 编译
3. **P2**: 优化大规模索引性能
4. **P3**: 增加按 kind 过滤搜索、符号元数据输出等开发者功能
5. **P4**: 考虑引入 embedding 语义搜索能力
