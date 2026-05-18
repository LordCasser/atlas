# Atlas 文档索引与当前架构决策

> 当前 `docs/` 已删除早期背景旧文档；本文是后续 Coder Agent 的唯一阅读入口。所有文档均代表当前有效方向：Rust-native、MVP 8 语言、query-driven extraction、SQLite + GraphSnapshot、MCP-first。

---

## 1. 当前最终方向

Atlas 的目标不是逐行迁移 CodeGraph，而是：

> **受 CodeGraph 启发，用 Rust 构建一个 local-first 的代码语义关系图谱引擎：快速本地分析代码库，将符号、作用域、引用、调用、类型和依赖关系持久化为图谱，并通过 MCP 为 LLM Agent 提供通用调用查询、依赖分析、影响面分析和未来污点分析能力。**

核心原则：

1. **Rust-native，不做 TypeScript 结构复刻**。
2. **MVP 聚焦 8 种语言**：C / C++ / Python / Java / ArkTS / TypeScript / JavaScript / Cangjie。
3. **保留 CodeGraph 的产品形态**：本地 AST 分析、SQLite 持久化、增量同步、MCP 工具。
4. **不照搬 CodeGraph 的大 `TreeSitterExtractor`**：改为 `tree-sitter query engine + LanguageAdapter`。
5. **不兼容 `.codegraph` DB schema**：`.atlas` schema 支持 symbols / scopes / references / edges / callsites。
6. **SQLite 是持久化源，GraphSnapshot 是查询加速层**。
7. **为调用分析和污点分析预留数据模型**：必须保留 reference occurrence、callsite、argument/parameter/return/assignment 等扩展点。
8. **所有非结构关系都必须可解释**：携带 confidence / provenance / resolved_by。

---

## 2. 推荐阅读顺序

### 2.1 CodeGraph 机制分析

- [`01-codegraph-analysis.md`](./01-codegraph-analysis.md)

说明 CodeGraph 如何抽取：

```text
symbol relationships
call graph
code structure
unresolved references
resolution
SQLite storage
MCP search/context/explore tools
```

阅读目的：理解 CodeGraph 的产品经验和可借鉴点，而不是照搬实现。

### 2.2 Rust-native 架构结论

- [`02-rust-native-mvp-architecture.md`](./02-rust-native-mvp-architecture.md)

当前最重要的架构文档，定义：

```text
Rust-native 分层
Core IR
SQLite schema
LanguageAdapter
tree-sitter query extraction
scope-aware resolution
GraphSnapshot
MCP tools
MVP milestones
```

### 2.3 MVP 语言计划

- [`03-mvp-language-plan.md`](./03-mvp-language-plan.md)

明确 8 种 MVP 语言的抽取范围、限制、resolution 策略和 fixture 验收。

### 2.4 当前需求规格

- [`04-current-requirements.md`](./04-current-requirements.md)

当前有效需求规格。后续实现和测试应以此作为功能/非功能验收依据。

### 2.5 实施计划

- [`05-implementation-plan.md`](./05-implementation-plan.md)

当前有效里程碑计划。替代早期 bottom-up CodeGraph parity 迁移计划。

### 2.6 模块契约

- [`06-module-contracts.md`](./06-module-contracts.md)

当前模块接口草案，给后续 Coder Agent 作为实现边界参考。

---

## 3. 对 Coder Agent 的强约束

如果你是后续负责实现的 Coder Agent：

1. **不要实现旧式大型 `GenericExtractor`。**
   - 正确方向：tree-sitter `.scm` queries + per-language `LanguageAdapter`。

2. **不要追求 23 种语言 feature parity。**
   - MVP 只做 C / C++ / Python / Java / ArkTS / TypeScript / JavaScript / Cangjie。

3. **不要复制 `.codegraph` schema。**
   - `.atlas` 必须保留 `references`、`scopes`、`callsites`，不只保存最终 `edges`。

4. **不要删除 resolved references。**
   - reference occurrence 是调用分析、污点分析、低置信度诊断的基础事实。

5. **不要让低置信度解析伪装成精确结果。**
   - 所有 semantic edge 必须有 confidence/provenance/resolved_by。

6. **不要让 MCP 图查询每一步都 hit SQLite。**
   - 使用 immutable `GraphSnapshot` 做 callers/callees/impact/path/neighbors。

7. **ArkTS MVP 先复用 TypeScript grammar。**
   - 但存储 language 必须是 `arkts`。

8. **C/C++ 是 best-effort。**
   - 不承诺完整宏展开、模板实例化、重载解析。

9. **Cangjie 先 grammar spike。**
   - 先验证 build、AST node kinds、minimal fixture，再扩展 adapter。

10. **MCP 是一等公民。**
    - 工具输出要 bounded、结构化、可解释。

---

## 4. 当前 docs 文件列表

```text
docs/
  00-docs-index.md
  01-codegraph-analysis.md
  02-rust-native-mvp-architecture.md
  03-mvp-language-plan.md
  04-current-requirements.md
  05-implementation-plan.md
  06-module-contracts.md
```

如需增加新文档，请保持编号顺序，并在本索引中注册。
