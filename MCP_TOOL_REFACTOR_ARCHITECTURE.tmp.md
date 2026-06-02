# Atlas MCP 工具重构 — 实现蓝图

> **目标**：33 工具 → 18 工具，breaking change，无别名兼容。
> **最终工具数量**：18 个。

---

## 1. 设计原则（实现约束）

1. **不保留旧工具 alias**，不维护 deprecated fallback。
2. **所有工具直接公开**，不设 agent/expert profile 分层。
3. **重复工具直接删除**，不隐藏或兼容。
4. **不使用 `atlas_` public 前缀**。
5. **后台异步统一为 `task`**，不再用 `job`。
6. **参数判别字段保留差异**：`action` / `view` / `direction` / `kind` 不统一。
7. **`symbol` 工具的主参数用 `qname`**（避免与工具名冲突）。

---

## 2. 最终 18 工具总览

```text
project        index
search         symbol
calls          explore      path          impact
file_dependencies
trace
lifecycle      branch_diff
fp_dispatches  domain_rules
tasks          task_status  wait_for_task  resume_task
```

### 分组

| 组 | 工具 |
|---|---|
| Project | `project`, `index` |
| Symbol | `search`, `symbol` |
| Graph / Impact | `calls`, `explore`, `path`, `impact` |
| File Graph | `file_dependencies` |
| Source Trace | `trace` |
| Semantic Analysis | `lifecycle`, `branch_diff` |
| Annotations / Rules | `fp_dispatches`, `domain_rules` |
| Tasks | `tasks`, `task_status`, `wait_for_task`, `resume_task` |

---

## 3. 删除 / 合并映射

### 3.1 Project → `project`

| 旧工具 | 处理 |
|---|---|
| `open_project` | `project(action="open")` |
| `status` | `project(action="status")` |
| `files` | `project(action="files")` |
| `language_capabilities` | 信息并入 `project(action="status")`，始终输出 |

`index` 保持独立（重型状态改变操作）。

### 3.2 Symbol → `symbol` + `search`

| 旧工具 | 处理 |
|---|---|
| `symbol` | `symbol(view="detail")` |
| `context` | `symbol(view="context")` |
| `usages` | `symbol(view="usages")` |

`explore` 保留独立——返回浅层 JSON（depth=1 邻接），`symbol(view="context")` 返回深层 JSON（call-site 细节、precision_tier 等），用途不同。

`search` 保持独立（发现入口 vs 已知符号查询）。

### 3.3 Graph → `calls` + `explore` + `path` + `impact`

| 旧工具 | 处理 |
|---|---|
| `callers` | `calls(direction="incoming")` |
| `callees` | `calls(direction="outgoing")` |
| `callgraph` | `calls(direction="both", depth>1)` |
| `neighbors` | `calls(edge_kinds=[...])` ← **必须实现 edge_kinds 参数** |
| `explore` | 保留独立 |

`calls` 必须实现 `edge_kinds` 参数（默认 `["calls","instantiates","implements"]`，支持 `["*"]`），以覆盖旧 `neighbors` 的非调用边查询能力。**不接受能力退化。**

### 3.4 File Graph → `file_dependencies`

| 旧工具 | 处理 |
|---|---|
| `dependencies` | `file_dependencies(direction="outgoing")` |
| `dependents` | `file_dependencies(direction="incoming")` |

删除 `file_id` 参数，仅保留 `file_path`。

### 3.5 Source Trace → `trace`

| 旧工具 | 新 kind |
|---|---|
| `trace_point` | `trace(kind="point")` |
| `trace_variable` | `trace(kind="variable")` |
| `trace_forward` | `trace(kind="forward")` |
| `trace_caller_path` | `trace(kind="callers")` |

四种 kind 底层使用不同引擎（TraceEngine / ForwardPathExplorer / CallerPathExplorer，均基于 DB 直接查询），与 `path` 的 GraphSnapshot 路径不同。**在 facade handler 中按 kind 分派，不要统一调用同一代码路径。**

### 3.6 Semantic Analysis

| 旧工具 | 新工具 |
|---|---|
| `atlas_lifecycle` | `lifecycle` |
| `atlas_branch_diff` | `branch_diff` |

纯重命名。

### 3.7 Annotations / Rules

| 旧工具 | 新工具 |
|---|---|
| `annotate_fp_dispatch` | `fp_dispatches(action="add")` |
| `list_fp_annotations` | `fp_dispatches(action="list")` |
| `delete_fp_annotation` | `fp_dispatches(action="delete")` |
| `atlas_annotate` | `domain_rules(action="add")` |
| `atlas_domain_rules` | `domain_rules(action="list|delete")` |
| `atlas_rule_learn` | `domain_rules(action="learn")` |

### 3.8 Tasks

| 旧工具 | 新工具 |
|---|---|
| `jobs` | `tasks` |
| `atlas_jobs` | `tasks` |
| `task_status` | `task_status` |
| `wait_for_task` | `wait_for_task` |
| `atlas_resume` | `resume_task` |

---

## 4. 工具 Schema 设计

---

### 4.1 `project`

```json
{
  "action": "open|status|files",
  "project_path": "optional string",
  "storage": "optional memory|persistent",
  "scan_files": "optional boolean",
  "background": "optional boolean",
  "verbose": "optional boolean",
  "limit": "optional integer",
  "language": "optional string",
  "path_prefix": "optional string"
}
```

**参数约束**：

```text
action=open:    required: project_path; optional: storage, scan_files, background
action=status:  optional: verbose
action=files:   optional: limit, language, path_prefix
```

**输出要求**：`action=status` 始终输出以下全部字段（无需 verbose 门控）：
active project, storage, db path, indexed files count, symbols count, edges count, references count, extraction layer status, lazy dataflow status, active task summary, per-language capability summary。

---

### 4.2 `index`

```json
{
  "include": ["optional glob"],
  "exclude": ["optional glob"],
  "background": "optional boolean"
}
```

保持独立（重型操作，常伴 background task）。

---

### 4.3 `search`

```json
{
  "query": "string",
  "scope": "optional string",
  "kind": "optional string",
  "limit": "optional integer",
  "background": "optional boolean",
  "include_roots": ["optional string"]
}
```

---

### 4.4 `symbol`

```json
{
  "qname": "string",
  "view": "detail|context|usages",
  "includeCode": "optional boolean",
  "limit": "optional integer",
  "include_roots": ["optional string"]
}
```

**参数约束**：

```text
view=detail:   returns kind, location, signature, caller/callee summaries
               optional: includeCode, include_roots

view=context:  returns 结构化 ContextView JSON（见 4.4.1）
               optional: includeCode, include_roots

view=usages:   returns references/usages/calls/instantiations
               optional: limit
```

#### 4.4.1 `symbol(view="context")` 结构化输出

去除 Markdown，改为结构化 JSON。可读性由 TUI 负责。

```json
{
  "symbol": "qname",
  "view": "context",
  "subject": { /* SymbolDef */ },
  "subject_file_path": "src/foo.rs",
  "subject_source": { "lines": [...], "start_line": 42, "total_lines": 3, "truncated": false },
  "caller_details": [
    { "symbol": {/* SymbolDef */}, "callsite_line": 10, "callsite_snippet": "  foo(x);", "edge_kind": "calls" }
  ],
  "callee_details": [
    { "symbol": {/* SymbolDef */}, "callsite_line": 43, "callsite_snippet": "  bar();", "edge_kind": "calls", "callee_signature": "void bar(int x)" }
  ],
  "file_peers": [ /* SymbolDef */ ],
  "importers": ["src/main.rs"],
  "dependencies": ["src/lib.rs"],
  "trail": {
    "calls": "symbol with view=context, symbol: \"...\"",
    "called_by": "trace with kind=callers, symbol: \"...\"",
    "full_source": "explore with includeCode=true, symbol: \"...\""
  },
  "precision_tier": "Exact",
  "hint": "...",
  "warnings": [],
  "query_id": "..."
}
```

**Trail 中旧工具名 → 新工具名映射**：

| 旧 Trail | 新 Trail |
|---|---|
| `context` with `symbol:` | `symbol` with `view=context, symbol:` |
| `trace_caller_path` with `symbol_name:` | `trace` with `kind=callers, symbol:` |
| `explore` or `codegraph_node(...)` | `explore` with `includeCode=true, symbol:` |

**实现要点**：
1. 改动范围：仅 `crates/atlas-mcp/src/tools/context.rs` 输出构建逻辑。
2. `SymbolDef` 已派生 `Serialize`，可直接 `serde_json::to_value`。
3. `CallerDetail` / `CalleeDetail` / `SourceSnippet` 未派生 `Serialize`，需手动构建 JSON 或加 serde 依赖。
4. `build_context_for_symbol()` 不变，仅改变序列化方式。

---

### 4.5 `calls`

```json
{
  "symbol": "string",
  "direction": "incoming|outgoing|both",
  "depth": "optional integer",
  "limit": "optional integer",
  "edge_kinds": ["optional string"]
}
```

**参数约束**：

```text
direction: incoming|outgoing|both
depth:     default 1。depth>1 覆盖旧 callgraph 场景
edge_kinds: default ["calls","instantiates","implements"]；支持 ["*"] 查所有边类型
```

**输出格式**：按 depth 分层。

```text
depth=1 → 扁平格式: {symbol, depth, callers/callees, total_*, query_id}
depth>1 → hop 格式:  {symbol, max_depth, hops[{depth, symbol, callers, callees}], ...}
```

**强制要求**：`edge_kinds` 非默认时 `node_json()` 须输出 `edge` 字段；`is_call_edge()` 替换为 `edge_kinds` 参数匹配。

---

### 4.6 `explore`

```json
{
  "symbol": "string",
  "includeCode": "optional boolean"
}
```

输出按 `edge_kind` 分组的 incoming/outgoing 数组（depth=1）。直接复用旧 `handle_explore`。

---

### 4.7 `path`

```json
{
  "from": "string",
  "to": "string",
  "max_depth": "optional integer",
  "direction": "optional outgoing|incoming|both",
  "prefer_production": "optional boolean",
  "edge_kinds": ["optional string"],
  "includeCode": "optional boolean",
  "include_roots": ["optional string"]
}
```

---

### 4.8 `impact`

```json
{
  "symbol": "string",
  "depth": "optional integer",
  "semantic": "optional boolean"
}
```

---

### 4.9 `file_dependencies`

```json
{
  "file_path": "string",
  "direction": "incoming|outgoing|both",
  "limit": "optional integer"
}
```

`file_path` 为 required（`file_id` 已删除）。

---

### 4.10 `trace`

```json
{
  "kind": "point|variable|forward|callers",
  "file_id": "optional string",
  "file_path": "optional string",
  "line": "optional integer",
  "column": "optional integer",
  "symbol": "optional string",
  "from": "optional string",
  "to": "optional string",
  "max_depth": "optional integer",
  "include_roots": ["optional string"]
}
```

**参数约束**：

```text
kind=point:     required: (file_id OR file_path), line, column; max_depth ignored
kind=variable:  required: (file_id OR file_path), line, column; optional: max_depth
kind=forward:   required: from, to; optional: max_depth
                实现: RawTraceEngine + ForwardPathExplorer (DB 直接查询)
kind=callers:   required: symbol; optional: max_depth
                实现: CallerPathExplorer (DB 直接查询 + 生产评分)
```

**⚠️ 实现约束**：四种 kind 底层引擎不同，facade handler 必须按 kind 分派到对应代码路径，不要统一处理。

---

### 4.11 `lifecycle`

```json
{
  "symbol": "string",
  "field": "string",
  "include_roots": ["optional string"]
}
```

---

### 4.12 `branch_diff`

```json
{
  "symbol": "string",
  "include_roots": ["optional string"]
}
```

---

### 4.13 `fp_dispatches`

```json
{
  "action": "add|list|delete",
  "field_qname": "optional string",
  "target_qname": "optional string",
  "annotation_id": "optional string",
  "confidence": "optional number"
}
```

```text
action=add:    required: field_qname, target_qname; optional: confidence
action=list:   required: none
action=delete: required: annotation_id OR field_qname
```

---

### 4.14 `domain_rules`

```json
{
  "action": "add|list|delete|learn",
  "rule_kind": "optional free_fn|alloc_fn|owned_pattern|cleanup_fn",
  "pattern": "optional string",
  "rule_id": "optional string",
  "source": "optional builtin|learned|user",
  "confidence": "optional number",
  "min_confidence": "optional number"
}
```

```text
action=add:    required: rule_kind, pattern; optional: confidence
action=list:   optional: source
action=delete: required: rule_id
action=learn:  optional: min_confidence
```

---

### 4.15 `tasks`

```json
{
  "query_id": "optional string"
}
```

聚合 TaskManager tasks + lazy extraction jobs + query refinement jobs，统一输出 `PublicTaskView`。

```rust
struct PublicTaskView {
    task_id: Option<String>,
    query_id: Option<String>,
    kind: String,        // index|search|open_project|lazy_extraction|query_refinement
    status: String,      // running|completed|failed
    progress: Option<f64>,
    message: Option<String>,
    elapsed_secs: Option<f64>,
}
```

---

### 4.16 `task_status`

```json
{ "task_id": "string" }
```

---

### 4.17 `wait_for_task`

```json
{
  "task_id": "string",
  "timeout_secs": "optional integer",
  "poll_interval_secs": "optional integer"
}
```

---

### 4.18 `resume_task`

```json
{ "query_id": "string" }
```

**⚠️ 实现约束**：`handle_resume()` 内有 14 个硬编码 match arm（`snapshot.tool_name` → `handle_*`）。重构后 tool_name 从旧名变为新名，arm 数缩减为约 7 个，但 facade handler 内部需从 `tool_args` 提取 `kind`/`direction`/`view` 二次分派。复杂度不消失，需仔细处理。

---

## 5. 实现架构

### 5.1 工具定义层

`make_all_tools()` 按领域拆分：

```rust
pub fn make_all_tools() -> Vec<Tool> {
    let mut tools = Vec::new();
    tools.extend(make_project_tools());     // project, index
    tools.extend(make_symbol_tools());      // search, symbol
    tools.extend(make_graph_tools());       // calls, explore, path, impact
    tools.extend(make_file_graph_tools());  // file_dependencies
    tools.extend(make_trace_tools());       // trace
    tools.extend(make_semantic_analysis_tools()); // lifecycle, branch_diff
    tools.extend(make_annotation_tools());  // fp_dispatches, domain_rules
    tools.extend(make_task_tools());        // tasks, task_status, wait_for_task, resume_task
    tools
}
```

### 5.2 Router dispatch

`ToolRouter::call_tool()` 只接受 18 个新工具名：

```rust
match name {
    "project" => self.handle_project(arguments),
    "index" => self.handle_index(arguments),
    "search" => self.handle_search(arguments),
    "symbol" => self.handle_symbol(arguments),
    "calls" => self.handle_calls(arguments),
    "explore" => self.handle_explore(arguments),
    "path" => self.handle_path(arguments),
    "impact" => self.handle_impact(arguments),
    "file_dependencies" => self.handle_file_dependencies(arguments),
    "trace" => self.handle_trace(arguments),
    "lifecycle" => self.handle_lifecycle(arguments),
    "branch_diff" => self.handle_branch_diff(arguments),
    "fp_dispatches" => self.handle_fp_dispatches(arguments),
    "domain_rules" => self.handle_domain_rules(arguments),
    "tasks" => self.handle_tasks(arguments),
    "task_status" => self.handle_task_status(arguments),
    "wait_for_task" => self.handle_wait_for_task(arguments),
    "resume_task" => self.handle_resume_task(arguments),
    _ => (format!("Unknown tool: {name}"), true),
}
```

### 5.3 内部 handler 复用

Facade handler 阶段性复用旧 handler：

```rust
// project
handle_project(action="open")   → handle_open_project
handle_project(action="status") → handle_status
handle_project(action="files")  → handle_files

// symbol
handle_symbol(view="detail")  → handle_symbol (旧)  [注意参数名从 symbol→qname]
handle_symbol(view="context") → handle_context      [输出改为结构化 JSON]
handle_symbol(view="usages")  → handle_usages

// calls
handle_calls(direction,incoming, depth=1)  → handle_callers
handle_calls(direction,outgoing, depth=1)  → handle_callees
handle_calls(depth>1)                      → handle_callgraph (is_call_edge→edge_kinds匹配)
handle_calls(edge_kinds≠default)           → handle_neighbors (adapted: node_json增加edge字段)

// trace — 必须保持独立代码路径
handle_trace(kind="point")    → handle_trace_point
handle_trace(kind="variable") → handle_trace_variable
handle_trace(kind="forward")  → handle_trace_forward
handle_trace(kind="callers")  → handle_trace_caller_path

// 简单重命名
handle_explore               → 直接复用
handle_file_dependencies     → handle_dependencies / handle_dependents
handle_lifecycle             → handle_atlas_lifecycle
handle_branch_diff           → handle_atlas_branch_diff
handle_fp_dispatches         → fp annotation handlers
handle_domain_rules          → domain rule handlers
handle_tasks                 → jobs + atlas_jobs 聚合
handle_resume_task           → handle_resume (需更新 match arm)
```

### 5.4 Graph 初始化

需要 graph snapshot 的工具：`symbol`, `calls`, `explore`, `path`, `impact`。

---

## 6. 迁移执行计划

### 阶段 1：更新工具定义

修改 `make_all_tools()`：
1. 删除所有 33 个旧工具定义。
2. 添加 18 个新工具定义（含 description、schema、required 字段）。
3. `calls` 必须包含 `edge_kinds` 参数。
4. `project(action="status")` 需包含全量状态信息。
5. `symbol` 参数名为 `qname`。
6. `file_dependencies` 无 `file_id`，`file_path` 为 required。

### 阶段 2：更新 dispatch

修改 `ToolRouter::call_tool()`：
1. 删除旧工具 match arm。
2. 添加 18 个新工具 match arm。
3. 不添加 alias 或 fallback。

### 阶段 3：新增 facade handlers

新增：`handle_project`, `handle_symbol`, `handle_calls`, `handle_file_dependencies`, `handle_trace`, `handle_fp_dispatches`, `handle_domain_rules`, `handle_tasks`, `handle_resume_task`。

避免命名冲突：`handle_symbol` → 旧 symbol handler 改为 `handle_symbol_detail`；`handle_domain_rules` → 如果冲突先重命名旧 handler。

### 阶段 4：同步内部命名

`handle_atlas_lifecycle` → `handle_lifecycle`
`handle_atlas_branch_diff` → `handle_branch_diff`
`handle_resume` → `handle_resume_task`

### 阶段 5：`symbol(view="context")` JSON 化

修改 `crates/atlas-mcp/src/tools/context.rs` 输出构建逻辑（见 4.4.1）。

### 阶段 6：更新测试与文档

- MCP tools/list golden tests
- MCP integration tests
- trace tests（扩展到 4 种 kind）
- lifecycle / branch diff / fp dispatch / domain rule tests
- README / AGENTS.md / MCP user docs

### 阶段 7：删除旧命名残留

确认以下字符串不再作为 MCP public tool name 出现：

```text
open_project, status, files, language_capabilities, context, usages,
callers, callees, callgraph, neighbors, dependencies, dependents,
trace_point, trace_variable, trace_forward, trace_caller_path,
jobs, atlas_jobs, atlas_resume,
annotate_fp_dispatch, list_fp_annotations, delete_fp_annotation,
atlas_lifecycle, atlas_branch_diff,
atlas_annotate, atlas_domain_rules, atlas_rule_learn
```

---

## 7. 实现风险

1. **`calls(edge_kinds=...)` 必须实现**：默认 `["calls","instantiates","implements"]`，支持 `["*"]`。`node_json()` 在非默认时输出 `edge` 字段。`handle_callgraph` 中 `is_call_edge()` 替换为 `edge_kinds` 匹配。
2. **`resume_task` 重分派**：`handle_resume()` 内 tool_name match 从旧名切换到新名（14 arm → ~7 arm），facade handler 内需二次分派 `kind`/`direction`/`view`。

---

## 附录 A：变更影响矩阵

| 新工具 | 替代旧工具 | 判别字段 | 复杂度 |
|---|---|---|---|
| `project` | open_project, status, files | `action` | ↑ 3→1 |
| `index` | index | — | → |
| `search` | search | — | → |
| `symbol` | symbol, context, usages | `view` | ↑ 3→1 |
| `calls` | callers, callees, callgraph, neighbors | `direction`, `edge_kinds` | ↑ 4→1 |
| `explore` | explore | — | → |
| `path` | path | — | → |
| `impact` | impact | — | → |
| `file_dependencies` | dependencies, dependents | `direction` | ↑ 2→1 |
| `trace` | trace_point, trace_variable, trace_forward, trace_caller_path | `kind` | ↑ 4→1 |
| `lifecycle` | atlas_lifecycle | — | → |
| `branch_diff` | atlas_branch_diff | — | → |
| `fp_dispatches` | 3 fp annotation tools | `action` | ↑ 3→1 |
| `domain_rules` | 3 domain rule tools | `action` | ↑ 3→1 |
| `tasks` | jobs, atlas_jobs | — | ↑ 2→1 |
| `task_status` | task_status | — | → |
| `wait_for_task` | wait_for_task | — | → |
| `resume_task` | atlas_resume | — | → |

---

## 附录 B：`calls` 合并 — 关键代码变更

| 变更 | 复杂度 | 说明 |
|---|---|---|
| `node_json()` 增加可选 `edge` 字段 | 低 | edge_kinds≠default 时输出 |
| `handle_callgraph` 中 `is_call_edge()` → `edge_kinds` 匹配 | 低 | 参数化过滤 |
| 新增 `handle_calls` facade handler | 中 | depth=1→扁平; depth>1→hop |
| `make_all_tools()` 删除 4 旧 / 新增 1 | 低 | callers,callees,callgraph,neighbors→calls |
| `call_tool()` dispatch | 低 | 4 arm→1 arm |
