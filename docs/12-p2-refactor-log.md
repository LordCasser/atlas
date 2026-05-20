# P2 重构日志：模块解析 + 调用图

> 完成时间: 2026-05-20
> 基于 P1 稳定基线 (ParseWorkerPool, SearchQueryParser, FileLock, GoldenTest)

---

## 变更概览

| 模块 | 变更 | 文件 |
|------|------|------|
| **图** | GraphBuilder 分离 (从 Resolver 移出 create_edges) | `src/graph/graph_builder.rs` |
| **图导出** | 导出 GraphBuilder, GraphBuilderStats | `src/graph/mod.rs` |
| **解析** | Resolver 重构 — resolve_all() 只返回 resolved facts | `src/resolution/mod.rs` |
| **解析** | PathAliasResolver (tsconfig.json paths) | `src/resolution/path_alias.rs` |
| **解析** | ExportResolver (re-export/barrel chains) | `src/resolution/export_resolver.rs` |
| **解析** | IncludeGraph (C/C++ include resolution) | `src/resolution/include_graph.rs` |
| **解析** | ImportResolver 增强 — 集成 PathAlias | `src/resolution/import_resolver.rs` |
| **解析导出** | 导出 PathAliasResolver, ExportResolver, IncludeGraph | `src/resolution/mod.rs` |
| **存储** | invalidate_references_for_file / delete_edges_for_file_references | `src/db/store.rs` |
| **同步** | SyncEngine 集成 GraphBuilder + resolved fact invalidation | `src/sync/mod.rs` |
| **CLI** | CLI 集成两步流程 (Resolver → GraphBuilder) | `src/cli/commands/index.rs` |
| **测试** | Integration tests PipelineStats 两步流程 | `tests/integration.rs` |
| **测试** | Golden test fixtures: TS imports + C includes | `tests/golden.rs`, `tests/fixtures/` |

---

## 核心架构变更

### 1. GraphBuilder 分离

**P0/P1**: `ReferenceResolver.resolve_all()` 既做解析，又创建 edges（`create_edges()` 方法）。

**P2**: 职责分离为两步流水线：

```
Step 1: ReferenceResolver.resolve_all()
         → (Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)
         → 更新 "references" 表的 resolved_* 列

Step 2: GraphBuilder.build_all(resolved)
         → GraphBuilderStats { edges_created, warnings }
         → 写入 edges 表
```

**设计决策**：
- `resolve_all()` 返回 `Vec<(ReferenceUse, ResolvedTarget)>` 而非 `Vec<(ReferenceId, ResolvedTarget)>`
  — GraphBuilder 需要完整的 ReferenceUse (kind, source_symbol) 来决定 edge 类型
- SyncEngine.sync() 和 CLI 都已集成两步流程

### 2. Resolved Fact Invalidation

新增两个 Store API，用于文件变更时的增量失效：

| API | 行为 |
|-----|------|
| `invalidate_references_for_file(file_id)` | 清除该文件中已解析引用的 resolved_* 列 |
| `delete_edges_for_file_references(file_id)` | 删除引用自该文件的所有 edges |

SyncEngine 集成：
- **删除文件**: `delete_edges_for_file_references()` + `delete_file_data()` (CASCADE)
- **修改文件**: `invalidate_references_for_file()` + `delete_edges_for_file_references()` + `delete_file_data()`
- 失效调用使用 `let _` (尽力清理，不中断主流程)

### 3. PathAliasResolver

解析 TypeScript tsconfig.json 的 `compilerOptions.paths` 映射：

```
@/components/Button → src/components/Button
@utils              → src/utils/index.ts
```

**解析算法** (优先级递减):
1. 精确别名匹配 (无通配符)
2. 通配符模式匹配 (最长前缀胜出)
3. baseUrl 前缀 (仅在无 paths 配置时)

**集成方式**: ImportResolver.candidate_qnames() 在生成候选 qname 前先调用 `path_alias.resolve(module)`

**关键决策**: 当 `paths` 已配置但无匹配时，裸说明符 (如 `lodash`) 视为外部 npm 包，不回退到 baseUrl

### 4. ExportResolver

解析 re-export/barrel 文件链：

```
consumer.ts        → import { Button } from './components'
./components/index → export { Button } from './Button'
./Button.ts        → export class Button { ... }
```

**算法**:
1. 在目标文件中查找 exported 符号 (by name)
2. 无直接匹配时，查找目标文件的 re-export imports 并递归
3. 递归深度限制 5，循环检测 (visited module set)

### 5. IncludeGraph

解析 C/C++ `#include` 指令：

```
#include "helper.h"   → project-relative / companion file
#include <stdio.h>    → 返回 None (系统头文件)
```

**策略**:
1. 尝试作为 project-relative 路径
2. 尝试 companion file (.c/.cpp/.cc/.cxx)
3. `find_includers(file_id)` — 反向 include 图

---

## 数据流图 (P2)

```
Source Files
     │
     ▼
 [extraction]  ──→  FileFacts  ──→  [db/Store]
                                       │
                                       ▼
 [resolution]  ──→  Vec<(ReferenceUse, ResolvedTarget)>
   ReferenceResolver                    │
   + PathAliasResolver                  ▼
   + ExportResolver              [db/Store] (resolved_* 更新)
   + IncludeGraph                       │
                                       ▼
 [graph/GraphBuilder]  ──→  edges 表
                                       │
                                       ▼
 [graph/GraphEngine]  ──→  GraphSnapshot (in-memory)
                                       │
                                       ▼
 [search] / [mcp] / [context]
```

---

## 新增测试

| 测试 | 位置 | 描述 |
|------|------|------|
| `test_graph_builder_basic` | graph_builder.rs | GraphBuilder 基本 edge 创建 |
| `test_cross_file_import_call_creates_edge` | resolution/mod.rs | 跨文件 import→call→edge |
| `test_cross_file_callers_callees_graph` | resolution/mod.rs | 跨文件 callers/callees |
| `test_empty_resolver` | path_alias.rs | 空 resolver 返回 None |
| `test_wildcard_pattern` | path_alias.rs | `@/*` → `src/*` |
| `test_exact_match` | path_alias.rs | 精确别名匹配 |
| `test_base_url_resolution` | path_alias.rs | baseUrl 前缀 |
| `test_longest_prefix_match` | path_alias.rs | 最长前缀胜出 |
| `test_reexport_resolution` | export_resolver.rs | re-export 链解析 |
| `test_include_graph_creation` | include_graph.rs | IncludeGraph 构造 |
| `test_system_include_returns_none` | include_graph.rs | 系统 include 不解析 |
| `test_candidate_qnames_from_import` | import_resolver.rs | FromImport 候选生成 |
| `test_path_alias_resolution` | import_resolver.rs | PathAlias 在 ImportResolver 中生效 |
| `golden_typescript_imports` | golden.rs | TS import golden test |
| `golden_c_includes` | golden.rs | C include golden test |

---

## 测试结果

- **184 passed, 0 failed**
- cargo check: clean
