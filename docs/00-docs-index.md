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

### 2.7 架构改进路线图

- [`../ARCHITECTURE_IMPROVEMENT_ROADMAP.md`](../ARCHITECTURE_IMPROVEMENT_ROADMAP.md)

定义 P0-P5 渐进式改进路线图：从正确性基线 → 产品化 → 模块解析 → DataFlow → 污点分析。

### 2.8 P1-P5 架构设计文档

- [`10-p1-p5-architecture-design.md`](./10-p1-p5-architecture-design.md)

**当前最重要的实现指导文档**。基于 ROADMAP 和 V2 文档的完整 P1-P5 架构设计：
- P1: 产品化 (ParseWorkerPool, FileLock, IndexReport, SearchQueryParser, GoldenTest)
- P2: 模块解析 (Resolver/GraphBuilder 分离, Import/Export/PathAlias, IncludeGraph)
- P3: BindingGraph + DataFlowGraph (BindingDef/BindingUse, DataNode/DataFlowEdge, CallsiteArgs)
- P4: CFG + 跨函数 Dataflow (CfgBuilder, Intra/Interprocedural, FunctionSummary)
- P5: 污点分析 (TaintRule, TaintEngine, TaintPath)

### 2.9 参考文档（历史/补充）

- [`07-架构变动文档.md`](./07-架构变动文档.md)
- [`08-Elixir项目分析与可参考实践.md`](./08-Elixir项目分析与可参考实践.md)
- [`09-p0-refactor-log.md`](./09-p0-refactor-log.md) — P0 重构实施日志
- [`11-p1-refactor-log.md`](./11-p1-refactor-log.md) — P1 产品化实施日志
- [`12-p2-refactor-log.md`](./12-p2-refactor-log.md) — P2 模块解析+调用图实施日志
- [`13-p3-refactor-log.md`](./13-p3-refactor-log.md) — P3 绑定图+数据流图实施日志

### 2.10 v2 全量重构方案（长期参考）

- [`../ARCHITECTURE_CLEAN_REFACTOR_V2.md`](../ARCHITECTURE_CLEAN_REFACTOR_V2.md)

描述 Atlas 面向未来的干净分层架构 (frontend/hir/semantic/facts/analysis)。P1-P5 渐进式方案对标 v2 目标。

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
  07-架构变动文档.md
  08-Elixir项目分析与可参考实践.md
  09-p0-refactor-log.md
  10-p1-p5-architecture-design.md
  11-p1-refactor-log.md
  12-p2-refactor-log.md
  13-p3-refactor-log.md
```

如需增加新文档，请保持编号顺序，并在本索引中注册。
