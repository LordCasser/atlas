## Why

首个 MCP Tasks 增量有意排除 `explore`：Focus retry 完成后仍可能得到多候选结果，而普通 completed task 无法让支持表单的客户端继续选择。rmcp 3.0.1 已能在运行中的 task 通过 `TaskContext::request_input()` 发布 `inputRequests`，并由 `tasks/update` 把响应送回同一个 task，因此现在可以在不暴露第二个 `query_id` 句柄、不新增控制面的前提下闭合这条生命周期。

本变更只处理一个窄场景：现代客户端同时声明 Tasks Extension 和表单 elicitation，`explore` 确实返回可绑定的 Focus retry ticket，且 task 重放最终得到现有多候选结果。其他 `explore`、MRTR 和兼容路径保持不变。

## What Changes

- 有效 MCP `2026-07-28+` 客户端同时声明 Tasks Extension 与表单 elicitation 时，真实且可绑定的 `explore` Focus retry ticket 可以转换为单一 task handle；缺任一能力时继续返回现有 `query_id` retry ticket。
- task 固定到产生 snapshot 的 `ActiveProject`，并同时捕获原始 `explore` 参数。重放得到多候选结果时，task 进入 `input_required`，复用现有候选表单的固定 input id、枚举值、标题与无 `requestState` 语义。
- 客户端通过 `tasks/update` 接受候选后，系统把选择作为普通、不可信的精确 `SymbolSelector` 写入原参数，并在固定项目上复用现有 `explore` 核心；若产生新的可绑定 retry ticket，同一 task 在原硬截止时间内继续等待，不创建嵌套 task 或暴露内部 query ID。
- 拒绝或取消候选输入时，task 以原完整候选结果完成；畸形响应使 task 以标准 JSON-RPC error 进入 `failed`。每个 task 最多发出一次候选输入请求。
- task 取消、300 秒硬截止、服务关闭、工具级错误、15 项目录/schema、直接 `ToolRouter::call_tool()` 以及非 task 的候选 MRTR 行为保持不变。

## Capabilities

### New Capabilities

<!-- 无。 -->

### Modified Capabilities

- `mcp-focus-query-tasks`: 允许同时具备 Tasks 与表单能力的 `explore` retry query 进入 task，并定义 task 内输入、后续重放与单句柄语义。
- `mcp-mrtr-candidate-selection`: 将现有候选表单与响应校验语义复用于 task 的 `input_required` / `tasks/update` 往返。

## Impact

- 影响 `crates/atlas-mcp/src/lib.rs` 的 task 创建门控、task 状态机、候选请求构造与响应解析。
- 影响 `crates/atlas-mcp/src/tools/mod.rs` 的固定 query snapshot 捕获，使 task 可安全保留原工具名与参数。
- 不升级 rmcp、不改变工具 schema、不新增持久化状态或推送订阅；仍只通过 `tasks/get` 观察状态、`tasks/update` 提交输入。
