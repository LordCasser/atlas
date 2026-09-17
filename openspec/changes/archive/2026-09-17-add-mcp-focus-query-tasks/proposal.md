## Why

Atlas 的 Focus 查询在交互预算内未完成时，会返回 `query_id`、`analysis.retry_after_ms`，再由客户端调用 `tasks` / `resume_query` 轮询和重放。这套自定义控制面保留了严格的“无部分语义结果”与终态 gap 规则，但支持 MCP Tasks Extension 的现代客户端无法使用标准 `CreateTaskResult`、`tasks/get` 和 `tasks/cancel`。

本变更选择一个不会让同一请求同时暴露两套进行中句柄的迁移切片：Atlas 仍先运行现有查询；只有结果确实是可恢复的 Focus retry ticket 时，现代 task-capable 客户端才收到单一标准 task handle，内部继续复用原 query snapshot。旧客户端仍收到原 `query_id` 流程。

## What Changes

- 服务器声明 `io.modelcontextprotocol/tasks`，并仅对有效 MCP `2026-07-28+` 且客户端声明该扩展的调用启用 task 结果。
- 除 `explore` 外，现有 Focus 工具调用若在交互预算后返回真实 `query_id` + `analysis.retry_after_ms`，且 snapshot 能绑定到产生它的活动项目，则将该中间结果隐藏并返回 `resultType: "task"`。`explore` 保留原流程，避免其重放后的候选 MRTR 被降级为普通 task 结果。
- task 在固定项目上通过现有 `resume_query` 路径等待并重放，直到得到现有终态成功、终态 gap 或工具错误；不复制 Focus 完成判定和 replay 逻辑。
- 实现标准 `tasks/get`、`tasks/update` 与 `tasks/cancel`。取消只停止该 task 的等待/重放；共享的 Focus 索引物化可继续完成并进入缓存。
- 旧协议、未声明 Tasks 扩展、终态调用、无法安全绑定 snapshot 的调用、MRTR 输入请求以及直接核心调用保持现有结果。
- 保留 15 项工具目录及 `tasks` / `resume_query` 兼容工具；同一个 task-capable 进行中调用不同时向客户端暴露 task ID 和 `query_id` retry ticket。

## Capabilities

### New Capabilities

- `mcp-focus-query-tasks`: 定义 Atlas Focus retryable 查询到 MCP Tasks Extension 的能力门控、项目绑定、轮询、取消和兼容语义。

### Modified Capabilities

<!-- 无。 -->

## Impact

- 影响 `crates/atlas-mcp/src/lib.rs` 的 rmcp 适配层与服务能力，及 `crates/atlas-mcp/src/tools/mod.rs` 的项目绑定 replay helper。
- 复用 rmcp 3.0.1 的 `TaskManager`，不升级依赖，不新增持久化数据库或跨进程任务恢复。
- task TTL 与现有 query snapshot 的 5 分钟 TTL 对齐；终态仍由现有 `CallToolResult` 表达。
- 本切片使用 `tasks/get` 轮询。rmcp 3.0.1 尚未把 task IDs 接入 `subscriptions/listen` 过滤与 `notifications/tasks` 路由，因此推送订阅留待独立变更。
