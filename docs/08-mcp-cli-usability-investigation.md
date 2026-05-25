# MCP 与 CLI 易用性优化调查

日期：2026-05-25

## 调查范围

本次调查覆盖 Atlas 面向用户和 Agent 的两个入口：

- CLI：`crates/atlas-cli/src/lib.rs`、`crates/atlas-cli/src/commands/*`
- MCP：`crates/atlas-mcp/src/lib.rs`、`crates/atlas-mcp/src/tools/*`
- 公开文档：`README.md`、`crates/atlas-cli/README.md`、`crates/atlas-mcp/README.md`、`docs/*`

目标是找出会阻断用户或 Agent 完成“安装 → 初始化/打开项目 → 索引 → 查询 → 追踪/依赖分析”闭环的问题，并给出可落地的优化优先级。

## 当前可用能力概览

### CLI

当前顶层命令：

- `init`
- `index`
- `sync`
- `status`
- `doctor`
- `files`
- `search`
- `context`
- `trace point`
- `trace variable`
- `trace caller-path`
- `mcp`

整体优点：

- `init` 已能提示大项目使用 `--analysis manifest`。
- `index` 已有增量 dirty-set、include/scope/exclude、阶段耗时、按语言统计。
- `search` 和 `trace` 已支持 `--json`。
- trace 系列有统一 `TraceQueryResponse` envelope，适合 Agent 消费。
- `mcp` 启动时会自动创建 `.atlas/atlas.db` 和 schema，降低首次启动门槛。

### MCP

精简后代码中实际注册 20 个工具：

- 项目/索引：`open_project`、`index`、`status`、`files`、`language_capabilities`
- 搜索/符号：`search`、`symbol`、`usages`
- 图查询：`neighbors`、`callers`、`callees`、`callgraph`、`path`、`impact`
- 上下文：`context`
- Trace：`trace_point`、`trace_variable`、`trace_caller_path`
- 文件依赖：`file_dependencies`
- 后台任务：`task_status`（可用 `wait=true` 等待完成）

整体优点：

- stdio MCP 启动时不立即构建图，graph/search/context 在首次图查询时 lazy 初始化，降低 handshake 超时风险。
- `open_project` 支持 memory/persistent 两种模式，能在 Agent 会话中切换活跃项目。
- `trace_point`、`trace_variable` 支持 `file_path`，不强依赖 opaque file id。
- `status` 会返回 active project、db path、storage、schema、compiled features、language capabilities。

## 关键易用性问题

### P0：工具输出的 ID 与后续工具输入不闭环

这是当前最影响 Agent 可用性的发现。

#### 1. `files` 必须返回短 `file_id`，以支撑 `file_dependencies`/trace 闭环

MCP `files` 当前只返回：

```json
{
  "path": "...",
  "language": "rust",
  "status": "success"
}
```

但 `file_dependencies` 的 schema 要求：

```json
{ "file_id": "..." }
```

结果是 Agent 无法从 `files` 的输出自然调用依赖工具。

建议：

- 对外统一使用短 hash：所有 MCP/CLI 展示型 `file_id` 都输出 `FileId::short_hex()`。
- `files` 增加短 `file_id` 字段。
- `file_dependencies`/trace 的 `file_id` 入参只接受短 hash；内部仍使用完整 32-byte `FileId`。

#### 2. `search`/`symbol` 已输出短 `file_id`，解析器也应统一接受短 hash

设计决策：对外 `file_id` 统一使用 `short_hex()`，例如：

```json
{
  "file_id": "737dbc4d"
}
```

因此后续工具必须能直接接受该短 hash；此前直接传给旧 `dependencies` 会得到：

```text
Invalid file_id: 737dbc4d
```

建议：

- 所有对外 `file_id` 输出保持短 hash。
- 所有 `file_id` 入参只接受 8 字符短 hash，不兼容 64 字符完整 hex。
- 若短 hash 不唯一，返回候选文件路径。

#### 3. `trace_caller_path --symbol` 声称可使用 search/symbol 的 symbol ID，但 search/symbol 未输出 symbol ID

CLI help 写着：

```text
Symbol ID in hex (from atlas_search or atlas_symbol)
```

MCP schema 也描述 `symbol` 来自 search/symbol；但实际 `search` 和 `symbol` 输出没有 `symbol_id`。

虽然 `trace_caller_path` 支持 `symbol_name`，但这不是稳定、唯一的定位方式。

建议：

- `search`、`symbol` 输出完整 `symbol_id` 和 `short_symbol_id`。
- CLI `atlas search --json` 也输出 `symbol_id`。
- `trace caller-path` 增加 `--qualified-name`，MCP 增加 `qualified_name` 入参。

### P0：MCP schema 与实现/文档存在不一致

这些不一致会直接误导 LLM Agent 生成错误工具调用。

#### 1. `index` handler 支持 `include`，但 tool schema 没暴露 `include`

`handle_index` 已读取：

```rust
args["include"].as_array()
```

但 `make_all_tools()` 的 `index.input_schema.properties` 只声明了 `analysis` 和 `exclude`，未声明 `include`。

建议：

- 在 `index` schema 增加：
  - `include: string[]`
  - 可选 `scope: string | string[]`，与 CLI 对齐。

#### 2. `open_project.index` 默认值描述互相矛盾

实现中：

```rust
let do_index = args["index"].as_bool().unwrap_or(false);
```

也就是默认 `false`。

但 schema 字段描述写的是 “default true”。这会让 Agent 以为打开项目后已经完成索引。

建议：

- 立即修正文案为 “default false”。
- 返回值中加入 `next_action`，例如未索引时明确提示：`call index` 或 `open_project(index=true, analysis="manifest")`。

#### 3. `task_status` 文案说支持 index/search 后台任务，但 index 未实现 `background`

`search` 实现了 `background: true`，`index` 没有解析 `background`，schema 也没声明。

建议：

- 要么给 `index` 补齐 `background`。
- 要么把 `task_status(wait=true)` 文案改成只服务 `search`。

#### 4. MCP 文档中的工具数量和命名过时

精简后实际工具是 20 个短名工具；但部分文档曾有：

- `crates/atlas-mcp/README.md`：写 “20 tools”。
- `docs/03-current-architecture.md`：写 “19 个 Agent-facing 工具”，且使用 `atlas_status`、`atlas_search`、`atlas_trace_point` 等旧前缀名。
- `docs/07-trace-contract.md` 仍引用 `atlas_trace_point` 等旧名称。

建议：

- 统一文档中的工具列表和命名。
- 如果已有用户/Agent 记住旧名，可考虑注册兼容 alias：`atlas_search -> search`、`atlas_trace_point -> trace_point`，并在结果中提示 deprecation。

### P1：CLI 缺少统一机器可读输出

当前只有 `search` 和 `trace` 有 `--json`；`status`、`files`、`doctor`、`index`、`sync` 没有 `--json`。

这会造成：

- Shell/CI 很难稳定消费状态。
- Agent 通过 CLI 使用 Atlas 时需要解析人类文本。
- CLI 与 MCP 的输出结构无法复用测试契约。

建议：

- 增加 global 或 per-command `--json`。
- 先覆盖高频命令：
  - `status --json`
  - `files --json`
  - `index --json`
  - `doctor --json`
- 输出建议统一 envelope：

```json
{
  "ok": true,
  "kind": "status",
  "result": {},
  "diagnostics": [],
  "next_actions": []
}
```

### P1：`--analysis` 和 MCP `analysis` 是自由字符串，拼错会静默退回 structural

CLI `index`/`sync`：

```rust
let mode = match analysis {
    "manifest" => ExtractionMode::Manifest,
    "full" => ExtractionMode::Full,
    _ => ExtractionMode::Structural,
};
```

MCP `index`/`open_project` 也是类似逻辑。

风险：

- 用户输入 `--analysis manfiest` 不报错，而是执行 structural，可能导致大项目耗时飙升。
- Agent 传错参数时无法自我纠正。

建议：

- CLI 使用 clap `ValueEnum`。
- MCP schema 使用 JSON Schema `enum: ["manifest", "structural", "full"]`。
- handler 对非法值返回结构化错误，不要静默 fallback。

### P1：MCP 输出缺少分页/限制，且截断可能破坏 JSON

`ToolRouter::call_tool` 对结果做字符级截断：

```rust
truncate(&result, 25000)
```

问题：

- `files` 当前返回所有文件；大项目会很容易超过 25KB。
- 字符级截断会把 JSON 截断成非法 JSON。
- Agent 得到的第一个 content block 可能无法解析，第二个 block 才提示 truncation。

建议：

- `files` 增加 `limit`、`offset`、`scope`。
- `language_capabilities` 可保持全量；其他集合类工具都应有 `limit` 和 `total_*`。
- 避免截断 JSON 字符串；改为数组级 limit，并返回：

```json
{
  "items": [],
  "total": 12345,
  "shown": 100,
  "truncated": true,
  "next_offset": 100
}
```

### P1：MCP progress 支持未覆盖实际会耗时的工具

`search`、`context`、`trace` handler 内都有 `send_progress(...)` 调用，但 MCP server 只在 `tool_name == "index"` 且有 progress token 时设置 progress channel。

因此这些工具的 progress 调用当前基本是 no-op。

建议：

- 对所有可能耗时工具设置 progress channel：
  - `index`
  - `search`
  - `context`
  - `trace_variable`
  - `trace_point`
  - `open_project(index=true)`
- 图首次初始化也应发送 progress，因为第一次 `search`/`context` 会触发 graph build。

### P1：项目路径发现策略不统一

MCP `atlas mcp --project .` 使用 `find_and_open`，会向上查找 `.atlas`。

但多数 CLI 读命令使用 `CommandContext::open(project, ExistingReadOnly)`，默认 `--project .` 时不会从子目录向上找 `.atlas`。

用户在项目子目录执行：

```bash
atlas status
```

可能失败，需要手动传 `--project ..` 或项目根。

建议：

- CLI 对默认 `--project .` 的只读命令也使用 ancestor walk。
- creator 命令如 `init` 仍保持显式当前目录语义，避免误初始化父目录。

### P2：CLI 帮助信息可以更任务导向

当前 `--help` 列出了命令和参数，但缺少常见工作流示例。

建议：

- 使用 clap `after_help` 为高频命令加入示例：
  - 首次使用：`atlas init && atlas index --analysis manifest`
  - 大项目局部索引：`atlas index --scope drivers/net`
  - Agent/MCP 配置：`atlas mcp --project /path/to/project`
  - trace：先 `atlas search --json` 再 `atlas trace caller-path --name ...`
- 增加 `atlas mcp config --client codex|claude|cursor|opencode` 生成配置片段，减少复制 README 的成本。

### P2：CLI/MCP 输出字段命名不统一

例子：

- CLI search JSON 是数组；MCP search 是 `{ query, count, results }`。
- CLI search 字段叫 `file_path`，MCP search 字段叫 `file`。
- CLI trace 用 envelope，CLI search/status/files 没有 envelope。

建议：

- 制定 V1 response contract：
  - `file_path` 作为规范字段。
  - `file_id` 始终为短 hash（`FileId::short_hex()`）。
  - 完整 64 hex 仅作为内部存储/调试字段，不进入默认 Agent-facing 输出。
  - 所有机器可读输出包含 `ok`、`kind`、`result`、`diagnostics`。

## 推荐优化路线

### 第一阶段：修闭环和 schema，一天内可完成

1. MCP `files` 输出短 `file_id`。
2. MCP/CLI 所有 `file_id` 输出统一为短 hash；`file_dependencies`、trace 入参只接受短 hash。
3. `file_dependencies` 可后续补充 `file_path` 作为辅助入口。
4. MCP `index` schema 增加 `include`；`open_project.index` 文案改成 default false。
5. `analysis` 参数改成 enum 校验。
6. 修正文档中的工具数量和旧 `atlas_` 前缀名。

### 第二阶段：统一机器输出和分页

1. `status --json`、`files --json`、`doctor --json`、`index --json`。
2. MCP `files` 增加 `limit/offset/scope`。
3. 所有列表型 MCP 工具返回 `total/shown/truncated/next_offset`。
4. 禁止字符级截断 JSON；保留兜底截断但只用于非 JSON markdown/text 输出。

### 第三阶段：降低首次使用成本

1. CLI 默认项目发现策略：只读命令从子目录向上找 `.atlas`。
2. `atlas mcp config --client ...` 生成配置。
3. MCP `open_project` 增加任务导向 preset：
   - `mode: "inspect"`：memory + manifest index
   - `mode: "persistent"`：persistent + structural index
   - `mode: "switch"`：只切换，不索引
4. 所有耗时 MCP 工具支持 progress。

## 建议验收用例

### MCP 闭环

1. `open_project(index=true, analysis="manifest")`
2. `status` 返回 `summary.files > 0`
3. `files(limit=1)` 返回短 `file_id`
4. `file_dependencies(file_id=<短 hash>, direction="outgoing")` 成功
5. `search("ToolRouter")` 返回完整 `symbol_id`
6. `trace_caller_path(symbol=...)` 成功或返回结构化 partial/error

### CLI 闭环

1. 在项目根执行：`atlas init && atlas index --analysis manifest`
2. 在子目录执行：`atlas status --json` 成功找到父级 `.atlas`
3. `atlas search ToolRouter --json` 输出完整 `symbol_id`
4. `atlas trace caller-path --symbol <symbol_id> --json` 成功
5. `atlas index --analysis manfiest` 明确失败并提示合法值

## 总结

Atlas 的 MCP/CLI 基础能力已经比较完整，主要问题不是“缺功能”，而是“Agent 调用闭环”和“契约一致性”：

- ID 输出与后续输入不匹配，是最高优先级。
- MCP schema、handler、README/docs 不一致，会误导 Agent。
- CLI 机器可读输出不统一，限制了 CI 和 Agent 场景。
- 大项目下需要分页、progress、结构化截断，避免超时和非法 JSON。

建议优先做 P0/P1 的契约修正，再考虑新增命令或高级交互。
