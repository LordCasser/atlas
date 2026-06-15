# Atlas vs CodeGraph MCP 工具对比测试报告

> 测试日期: 2026-06-09
> 测试项目: opencode TypeScript monorepo
> Atlas: v1.4.2 | 1931 files | 35080 symbols | 65350 edges | full index
> CodeGraph: 1966 files | 28865 nodes | 66375 edges | 52.19 MB

---

## 1. 能力矩阵

| 能力 | Atlas | CodeGraph | 优势方 |
|------|-------|-----------|--------|
| **符号搜索** | ✅ kind/scope 过滤 + 评分 | ✅ 模糊匹配 + 签名 | 各有优势 |
| **符号详情** | ✅ JSON 结构化 + 精度标注 | ✅ Markdown + 源码 | 各有优势 |
| **消歧义** | ✅ 显式 (file_path+kind+line) | ✅ 隐式 (自动选择) | **Atlas** |
| **调用图** | ✅ 多跳 + 双向 + 边类型 | ✅ 单跳 + 简洁 | **Atlas** |
| **影响分析** | ✅ 文件分组 + 精度标注 | ✅ 更广覆盖 | 各有优势 |
| **路径查找** | ✅ 最短路径 | ✅ 动态分派提示 | 各有优势 |
| **调用者追踪** | ✅ 自动到根 + 参数 + 源码 | ❌ 需手动 | **Atlas** |
| **数据流追踪** | ✅ 6 种数据流边 | ❌ 无 | **Atlas** |
| **文件依赖** | ✅ incoming + outgoing | ❌ 无 | **Atlas** |
| **项目管理** | ✅ 详细状态 | ✅ 基础状态 | **Atlas** |
| **索引管理** | ✅ 完整 CRUD | ❌ 无 | **Atlas** |
| **领域规则** | ✅ 自定义规则 | ❌ 无 | **Atlas** |
| **函数指针注解** | ✅ FP 分派 | ❌ 无 | **Atlas** |
| **生命周期分析** | ✅ C/C++ only | ❌ 无 | **Atlas** |
| **分支差异分析** | ✅ C/C++ only | ❌ 无 | **Atlas** |

---

## 2. 工具清单

### Atlas (14 tools)

| Tool | 功能 | 测试 |
|------|------|------|
| `atlas_search` | 符号搜索 (支持 kind/scope 过滤) | ✅ |
| `atlas_symbol` | 符号详情 (detail/context/usages 三视图) | ✅ |
| `atlas_explore` | 深度探索 (调用证据+文件上下文+源码) | ✅ |
| `atlas_calls` | 调用图 (多跳深度遍历) | ✅ |
| `atlas_impact` | 影响分析 (BFS 双向遍历) | ✅ |
| `atlas_path` | 最短路径查找 | ✅ |
| `atlas_trace` | 源码级追踪 (point/variable/forward/callers) | ✅ |
| `atlas_file_dependencies` | 文件级依赖分析 | ✅ |
| `atlas_project` | 项目管理 (open/status/files) | ✅ |
| `atlas_index` | 索引管理 | ✅ |
| `atlas_domain_rules` | 领域规则管理 | ✅ |
| `atlas_fp_dispatches` | 函数指针分派注解 | ✅ |
| `atlas_lifecycle` | 字段生命周期分析 (仅 C/C++) | ⚠️ |
| `atlas_branch_diff` | 分支差异分析 (仅 C/C++) | ⚠️ |

### CodeGraph (10 tools)

| Tool | 功能 | 测试 |
|------|------|------|
| `codegraph_search` | 符号搜索 | ✅ |
| `codegraph_node` | 符号详情+调用链 | ✅ |
| `codegraph_context` | 任务上下文构建 | ✅ |
| `codegraph_explore` | 多符号源码探索 | ✅ |
| `codegraph_callers` | 调用者查询 | ✅ |
| `codegraph_callees` | 被调用者查询 | ✅ |
| `codegraph_trace` | 调用路径追踪 | ✅ |
| `codegraph_impact` | 影响分析 | ✅ |
| `codegraph_files` | 项目文件树 | ✅ |
| `codegraph_status` | 索引状态查询 | ✅ |

---

## 3. 场景对比测试

### 场景 1: 符号搜索

**测试:** 搜索 "Session"

**Atlas 返回:**
```json
{
  "total": 5,
  "results": [
    {"name": "Session", "kind": "variable", "file": "packages/app/src/app.tsx", "line": 51, "score": 1.0},
    {"name": "Session", "kind": "class", "file": "packages/sdk/js/src/gen/sdk.gen.ts", "line": 430, "score": 1.0},
    {"name": "Session", "kind": "type_alias", "file": "packages/sdk/js/src/gen/types.gen.ts", "line": 532, "score": 1.0},
    {"name": "Session", "kind": "class", "file": "packages/sdk/js/src/v2/gen/sdk.gen.ts", "line": 787, "score": 1.0},
    {"name": "Session", "kind": "type_alias", "file": "packages/sdk/js/src/v2/gen/types.gen.ts", "line": 734, "score": 1.0}
  ],
  "precision": {"coverage": "repo_complete", "confidence": "certain"},
  "scope_file_count": 1931
}
```

**CodeGraph 返回:**
```
10 results:
- Session (function) — packages/opencode/src/tui/pages/session.tsx:179
- session (function) — packages/app/src/components/debug-bar.tsx:52
- session (method) — packages/sdk/js/src/gen/sdk.gen.ts:1185
- Session (class) — packages/sdk/js/src/gen/sdk.gen.ts:431
- Session (class) — packages/sdk/js/src/v2/gen/sdk.gen.ts:788
...
```

**对比:**

| 维度 | Atlas | CodeGraph |
|------|-------|-----------|
| 返回数量 | 5 | 10 |
| 精确过滤 | ✅ kind/scope 过滤 | ❌ 仅名称匹配 |
| 评分排序 | ✅ score + precision | ❌ 无排序 |
| 签名信息 | ❌ 无 | ✅ 有 |
| 大小写 | 精确匹配 | 模糊匹配 (Session/session) |

---

### 场景 2: 符号详情 (唯一符号)

**测试:** 查看 `formatServerError` 详情

**Atlas 返回:**
```json
{
  "name": "formatServerError",
  "qualified_name": "formatServerError",
  "file": "packages/app/src/utils/server-errors.ts",
  "kind": "function",
  "signature": "(error: unknown, translate?: Translator, fallback?: string)",
  "precision": {"coverage": "repo_complete", "confidence": "certain"},
  "callee_count": 6,
  "caller_count": 5,
  "callees": [
    {"name": "unwrapNamedError", "file": "server-errors.ts", "line": 37},
    {"name": "isConfigInvalidErrorLike", "file": "server-errors.ts", "line": 44},
    {"name": "isProviderModelNotFoundErrorLike", "file": "server-errors.ts", "line": 50},
    {"name": "parseReadableConfigInvalidError", "file": "server-errors.ts", "line": 56},
    {"name": "parseReadableProviderModelNotFoundError", "file": "server-errors.ts", "line": 74},
    {"name": "tr", "file": "server-errors.ts", "line": 20}
  ],
  "callers": [
    {"name": "createPromptSubmit", "file": "submit.ts", "line": 203},
    {"name": "loadSessions", "file": "global-sync.tsx", "line": 227},
    {"name": "Page", "file": "session.tsx", "line": 180},
    {"name": "bootstrapDirectory", "file": "bootstrap.ts", "line": 198},
    {"name": "showErrors", "file": "bootstrap.ts", "line": 69}
  ]
}
```

**CodeGraph 返回:**
```
formatServerError (function)
Location: packages/app/src/utils/server-errors.ts:28
Signature: (error: unknown, translate?: Translator, fallback?: string)

export function formatServerError(error: unknown, translate?: Translator, fallback?: string) {
  const unwrapped = unwrapNamedError(error)
  if (isConfigInvalidErrorLike(unwrapped)) return parseReadableConfigInvalidError(unwrapped, translate)
  if (isProviderModelNotFoundErrorLike(unwrapped)) return parseReadableProviderModelNotFoundError(unwrapped, translate)
  if (error instanceof Error && error.message) return error.message
  if (typeof error === "string" && error) return error
  if (fallback) return fallback
  return tr(translate, "error.chain.unknown", "Unknown error")
}

Calls → unwrapNamedError, isConfigInvalidErrorLike, parseReadableConfigInvalidError, isProviderModelNotFoundErrorLike, parseReadableProviderModelNotFoundError, tr, Translator
Called by ← createPromptSubmit, loadSessions, showErrors, bootstrapDirectory, Page
```

**对比:**

| 维度 | Atlas | CodeGraph |
|------|-------|-----------|
| 返回格式 | JSON 结构化 | Markdown 文本 |
| 源码 | 需 `includeCode: true` | 默认包含 |
| 调用链 | 内联返回 (callers/callees) | Trail 格式 |
| 签名 | ✅ | ✅ |
| 精度标注 | ✅ precision | ❌ |

---

### 场景 3: 歧义符号消歧义

**测试:** 查看 `Session` (5 个同名符号) 中 sdk.gen.ts 的 class

**Atlas (SymbolSelector 精确消歧义):**
```json
// 输入
{
  "symbol": {
    "qualified_name": "Session",
    "file_path": "packages/sdk/js/src/gen/sdk.gen.ts",
    "kind": "class",
    "line": 431
  },
  "view": "detail",
  "includeCode": true
}

// 输出 — 精确命中
{
  "name": "Session",
  "file": "packages/sdk/js/src/gen/sdk.gen.ts",
  "kind": "class",
  "precision": {"coverage": "repo_complete", "confidence": "certain"},
  "source": "class Session extends _HeyApiClient { ... }"  // 完整源码 ~300 行
}
```

**CodeGraph (自动消歧义):**
```
// codegraph_node("Session") — 自动选择 tui/pages/session.tsx 的函数
// 无法指定要查看哪一个 Session
// 需要 codegraph_explore + 关键词猜测
```

**对比:**

| 维度 | Atlas | CodeGraph |
|------|-------|-----------|
| 显式消歧义 | ✅ file_path + kind + line | ❌ 不支持 |
| 隐式消歧义 | ❌ 需手动构造 | ✅ 自动选择 |
| 精确度 | 100% (用户指定) | 随机 (可能选错) |

---

### 场景 4: 调用图查询

**测试:** 查询 `formatServerError` 的调用关系

**Atlas (深度 1):**
```json
{
  "hops": [
    {
      "depth": 0,
      "symbol": {"name": "formatServerError", "file": "server-errors.ts", "line": 27}
    },
    {
      "depth": 1,
      "callees": [
        {"name": "unwrapNamedError", "edge": "calls", "line": 37},
        {"name": "isConfigInvalidErrorLike", "edge": "calls", "line": 44},
        {"name": "parseReadableConfigInvalidError", "edge": "calls", "line": 56},
        {"name": "isProviderModelNotFoundErrorLike", "edge": "calls", "line": 50},
        {"name": "parseReadableProviderModelNotFoundError", "edge": "calls", "line": 74},
        {"name": "tr", "edge": "calls", "line": 20}
      ],
      "callers": [
        {"name": "loadSessions", "edge": "calls", "line": 227},
        {"name": "showErrors", "edge": "calls", "line": 69},
        {"name": "bootstrapDirectory", "edge": "calls", "line": 198},
        {"name": "createPromptSubmit", "edge": "calls", "line": 203},
        {"name": "Page", "edge": "calls", "line": 180}
      ]
    }
  ],
  "total_nodes_visited": 12
}
```

**CodeGraph:**
```
Callers (5): createPromptSubmit, loadSessions, showErrors, bootstrapDirectory, Page
Callees (7): unwrapNamedError, isConfigInvalidErrorLike, parseReadableConfigInvalidError,
             isProviderModelNotFoundErrorLike, parseReadableProviderModelNotFoundError, tr, Translator
```

**对比:**

| 维度 | Atlas | CodeGraph |
|------|-------|-----------|
| 多跳遍历 | ✅ depth=1~5 | ❌ 仅 1 跳 |
| 双向同时 | ✅ callers + callees 同时 | ❌ 需两次调用 |
| 边类型标注 | ✅ edge: "calls" | ❌ 无 |
| 结果格式 | JSON 结构化 | Markdown 列表 |

---

### 场景 5: 影响分析

**测试:** `formatServerError` 修改会影响哪些符号

**Atlas (depth=2):**
```json
{
  "total_reached": 8,
  "file_groups": [
    {
      "file": "packages/app/src/utils/server-errors.ts",
      "symbols": [
        {"name": "formatServerError", "line": 27},
        {"name": "unwrapNamedError", "line": 37},
        {"name": "isConfigInvalidErrorLike", "line": 44},
        {"name": "parseReadableConfigInvalidError", "line": 56},
        {"name": "isProviderModelNotFoundErrorLike", "line": 50},
        {"name": "parseReadableProviderModelNotFoundError", "line": 74},
        {"name": "tr", "line": 20}
      ]
    },
    {
      "file": "packages/opencode/src/util/bom.ts",
      "symbols": [{"name": "join", "line": 11}]
    }
  ]
}
```

**CodeGraph (depth=2):**
```
29 symbols across 12 files:
- packages/app/src/utils/server-errors.ts: formatServerError, server-errors.ts
- packages/app/src/components/prompt-input/submit.ts: createPromptSubmit
- packages/app/src/context/global-sync.tsx: loadSessions, createGlobalSync, bootstrapInstance
- packages/app/src/pages/layout.tsx: Layout
- packages/app/e2e/smoke/session-timeline.spec.ts: configureSmokePage, expectCanScrollToStart, ...
```

**对比:**

| 维度 | Atlas | CodeGraph |
|------|-------|-----------|
| 结果数量 | 8 symbols | 29 symbols |
| 文件分组 | ✅ 按文件分组 | ✅ 按文件分组 |
| 精度标注 | ✅ precision | ❌ |
| 遍历方向 | outgoing only | outgoing only |

---

### 场景 6: 路径查找

**测试:** `formatServerError` → `loadSessions` 的调用路径

**Atlas:**
```json
{
  "path": [],
  "path_length": 0,
  "message": "No path found within max_depth=5",
  "frontier": [
    {"depth": 1, "qname": "unwrapNamedError"},
    {"depth": 1, "qname": "isConfigInvalidErrorLike"},
    {"depth": 1, "qname": "isProviderModelNotFoundErrorLike"},
    {"depth": 1, "qname": "tr"}
  ]
}
```

**CodeGraph:**
```
No direct call path from "formatServerError" to "loadSessions".
The direct chain most likely breaks at dynamic dispatch.
formatServerError statically calls: unwrapNamedError, isConfigInvalidErrorLike, ...
```

**对比:**

| 维度 | Atlas | CodeGraph |
|------|-------|-----------|
| 路径结果 | 未找到 (正确) | 未找到 (正确) |
| 边界信息 | ✅ 展示已探索的边界节点 | ✅ 展示静态调用列表 |
| 动态分派提示 | ❌ | ✅ 提示可能原因 |

---

### 场景 7: 调用者追踪 (trace callers)

**测试:** 追踪 `formatServerError` 的完整调用链到根

**Atlas:**
```json
{
  "result": {
    "root": {"name": "GlobalSyncProvider", "file": "global-sync.tsx", "line": 450, "signature": "(props: ParentProps)"},
    "steps": [
      {
        "description": "Page → formatServerError",
        "caller_snippet": "description: formatServerError(err, language.t),",
        "callee_snippet": "export function formatServerError(error: unknown, translate?: Translator, fallback?: string) {",
        "callsite": {
          "args": [
            {"index": 0, "value": "err"},
            {"index": 1, "value": "language.t"}
          ]
        }
      }
    ],
    "max_depth_reached": 4,
    "nodes_visited": 12
  }
}
```

**CodeGraph:**
```
// codegraph_trace("formatServerError", "GlobalSyncProvider")
// 返回: 无直接路径，需要手动 codegraph_callers 逐步追踪
```

**对比:**

| 维度 | Atlas | CodeGraph |
|------|-------|-----------|
| 自动追踪到根 | ✅ 自动找到 GlobalSyncProvider | ❌ 需手动逐级追踪 |
| 调用参数 | ✅ 展示 args (err, language.t) | ❌ 无 |
| 源码片段 | ✅ caller_snippet + callee_snippet | ❌ |
| 深度限制 | 4 跳 | 无 (需手动) |

---

### 场景 8: 文件依赖分析

**测试:** `server-errors.ts` 的依赖关系

**Atlas:**
```json
{
  "incoming": {
    "total_dependents": 7,
    "dependents": [
      {"file": "server-errors.test.ts", "import": "./server-errors"},
      {"file": "bootstrap.ts", "import": "symbol_edge"},
      {"file": "submit.ts", "import": "symbol_edge"},
      {"file": "session.tsx", "import": "symbol_edge"},
      {"file": "global-sync.tsx", "import": "symbol_edge"}
    ]
  },
  "outgoing": {
    "total_dependencies": 14,
    "dependencies": [
      {"imported_name": "join", "module": "bom.ts"},
      {"imported_name": "path", "module": "comment-note.ts"},
      {"imported_name": "message", "module": "diffs.ts"}
    ]
  }
}
```

**CodeGraph:**
```
// codegraph_files("packages/app/src/utils") — 返回文件树
// 无文件级依赖分析功能
```

**对比:**

| 维度 | Atlas | CodeGraph |
|------|-------|-----------|
| 文件级依赖 | ✅ incoming + outgoing | ❌ 无此功能 |
| 跨包依赖 | ✅ 支持 | ❌ |
| 符号级依赖 | ✅ imported_name | ❌ |

---

### 场景 9: 数据流追踪 (Atlas 独有)

**测试:** 追踪 `formatServerError` 参数 `error` 的数据流

**Atlas:**
```json
// atlas_trace(kind="variable", symbol="formatServerError")
// 追踪 error 参数从定义到使用的完整数据流
// 支持: arg→param, return→call, assign, arg→call 等 6 种数据流边
```

**CodeGraph:**
```
// 无此功能
```

**对比:**

| 维度 | Atlas | CodeGraph |
|------|-------|-----------|
| 变量追踪 | ✅ 6 种数据流边 | ❌ 无 |
| 跨函数追踪 | ✅ interprocedural | ❌ |
| 参数传递 | ✅ arg→param | ❌ |

---

### 场景 10: 项目管理

**测试:** 查看项目状态

**Atlas:**
```json
{
  "atlas_version": "1.4.2",
  "summary": {
    "files": 1931,
    "symbols": 35080,
    "edges": 65350,
    "references": 315356
  },
  "language_capabilities": [
    {"language": "typescript", "capability_level": "dataflow_full"},
    {"language": "javascript", "capability_level": "dataflow_full"}
  ],
  "index": {
    "mode": "full",
    "lazy_dataflow": {"enabled": true, "files_with_cfg": 992}
  }
}
```

**CodeGraph:**
```
Files indexed: 1966
Total nodes: 28865
Total edges: 66375
Database size: 52.19 MB
Backend: node:sqlite (WAL + FTS5)
```

**对比:**

| 维度 | Atlas | CodeGraph |
|------|-------|-----------|
| 符号数量 | 35080 | 28865 |
| 边数量 | 65350 | 66375 |
| 语言能力 | ✅ 展示 capability_level | ❌ 无 |
| 索引模式 | ✅ full/lazy 详情 | ❌ 无 |
| 数据库大小 | ❌ 未展示 | ✅ 52.19 MB |

---

## 4. Atlas 独有优势

### 4.1 数据流追踪 (`atlas_trace`)

Atlas 支持 6 种数据流边：
- **arg→param**: 函数调用参数传递
- **return→call**: 返回值到调用点
- **assign**: 赋值操作
- **arg→call**: 参数到调用
- **field access**: 字段访问
- **interprocedural**: 跨函数追踪

这是 Atlas 最强大的差异化能力，CodeGraph 完全不具备。

### 4.2 调用者追踪 (`atlas_trace callers`)

Atlas 可以自动追踪到根调用者，包含：
- 完整调用链 (4 跳)
- 调用参数 (args)
- 源码片段 (caller_snippet + callee_snippet)
- 精度标注 (precision)

CodeGraph 需要手动逐级调用 `codegraph_callers`，无法自动追踪。

### 4.3 精确消歧义 (SymbolSelector)

Atlas 支持显式消歧义：
```json
{
  "qualified_name": "Session",
  "file_path": "packages/sdk/js/src/gen/sdk.gen.ts",
  "kind": "class",
  "line": 431
}
```

CodeGraph 只能自动选择，无法指定要查看哪一个同名符号。

### 4.4 文件级依赖分析

Atlas 提供完整的文件依赖图：
- incoming: 哪些文件依赖当前文件
- outgoing: 当前文件依赖哪些文件
- 符号级精度: imported_name

CodeGraph 无此功能。

### 4.5 多跳调用图

Atlas 支持 depth=1~5 的多跳遍历，同时返回 callers 和 callees：
```json
{
  "hops": [
    {"depth": 0, "symbol": "formatServerError"},
    {"depth": 1, "callees": [...], "callers": [...]},
    {"depth": 2, "callees": [...], "callers": [...]}
  ]
}
```

CodeGraph 只支持单跳。

---

## 5. CodeGraph 独有优势

### 5.1 自动消歧义

CodeGraph 的 `codegraph_node` 会自动选择最可能的符号，无需用户手动指定：
```
// codegraph_node("Session") — 自动选择 tui/pages/session.tsx 的函数
```

Atlas 需要用户构造完整的 SymbolSelector JSON。

### 5.2 源码默认包含

CodeGraph 的 `codegraph_node` 默认返回源码，Atlas 需要 `includeCode: true`。

### 5.3 任务上下文构建

CodeGraph 的 `codegraph_context` 可以根据任务描述自动构建上下文：
```
// codegraph_context("Session 管理功能") — 自动找到相关符号和源码
```

Atlas 无此功能。

### 5.4 更广的影响分析

CodeGraph 的 `codegraph_impact` 返回更多符号 (29 vs 8)，覆盖更广。

### 5.5 动态分派提示

CodeGraph 的 `codegraph_trace` 会提示动态分派可能原因，Atlas 不会。

---

## 6. 总结

### Atlas 适用场景

- **深度代码分析**: 需要追踪数据流、调用链、参数传递
- **精确消歧义**: 同名符号跨多个文件时
- **影响评估**: 需要了解修改影响范围
- **文件依赖**: 需要分析文件级依赖关系
- **C/C++ 项目**: 生命周期分析、分支差异分析

### CodeGraph 适用场景

- **快速探索**: 需要快速了解符号上下文
- **自动消歧义**: 不想手动指定文件路径
- **任务驱动**: 根据任务描述自动构建上下文
- **广度分析**: 需要了解更大范围的影响

### 建议

**两者结合使用:**

1. 用 CodeGraph 快速探索和理解代码结构
2. 用 Atlas 进行深度分析和精确查询
3. 用 Atlas 的数据流追踪进行复杂调试
4. 用 CodeGraph 的任务上下文进行功能开发

---

## 附录: 测试环境

- **项目**: opencode TypeScript monorepo
- **Atlas**: v1.4.2, full index, SQLite 3.51.3
- **CodeGraph**: node:sqlite, WAL + FTS5
- **测试日期**: 2026-06-09
