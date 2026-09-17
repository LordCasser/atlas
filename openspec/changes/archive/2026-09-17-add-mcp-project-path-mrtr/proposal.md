## Why

`project(action="open")` 的路径只在该 action 下必需，现有统一工具 schema 无法在不改变冻结 V1 验证语义的前提下把它声明为全局 required；当客户端省略 `project_path` 时，Atlas 只能返回终止错误。MCP 2026-07-28 的 MRTR 可以只在这个调用真正缺少路径时请求用户补充，并继续复用现有项目打开校验与副作用边界。

## What Changes

- 对已协商 MCP `2026-07-28` 或更高日期版本、且声明支持表单 elicitation 的客户端，当 `project(action="open")` 缺少或给出空 `project_path` 时返回标准 `input_required` 路径表单。
- 接受非空路径后，将其写回原请求的 `project_path`，保留其余参数，并通过现有 `handle_open_project()` 路径执行 canonicalize、长度、目录、数据库与 schema 校验。
- 对旧协议、未声明表单 elicitation、非 `open` action、已有非空路径、错误类型的路径，以及用户拒绝或取消补充的流程，保持现有完整工具结果语义。
- 该流程不发出 `requestState`，不保存路径或服务器进度；客户端重试必须保留原参数，注入的 state 与畸形响应返回 `invalid_params`。
- 不改变 15 项工具目录、`project.inputSchema`、`ToolRouter::call_tool()` 的核心结果格式或其他工具行为。

## Capabilities

### New Capabilities

- `mcp-mrtr-project-path`: 定义版本与客户端能力感知的 `project(open)` 缺失路径表单、重试和兼容回退语义。

### Modified Capabilities

<!-- 无。 -->

## Impact

- 影响 `crates/atlas-mcp/src/lib.rs` 的 rmcp 传输适配层及其单元/wire 回归测试。
- 复用 rmcp 3.0.1 现有 MRTR 与字符串 elicitation schema，不升级依赖，不新增持久化状态。
- 项目打开仍由现有同步核心处理；其文件系统与 SQLite 副作用、错误消息和安全校验不在本变更中重定义。