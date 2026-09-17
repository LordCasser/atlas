## Context

Atlas 的工具核心是同步 `ToolRouter::call_tool()`。Focus 驱动的 handler 会创建 5 分钟 query snapshot；外层 `settle_focus_response()` 在单次交互预算内等待，完成则重放原查询，超时则只返回严格 retry ticket，不发布临时语义结果。`resume_query` 绑定 snapshot 中的原工具名、参数和 `FocusResult`，并负责生成终态成功、失败 gap 或下一轮 retry 状态。

rmcp 3.0.1 提供 `TaskManager`、`CreateTaskResult`、`tasks/get`、`tasks/update` 与协作取消。Tasks Extension 对 MCP `2025-11-25` 未定义，因此 Atlas 还需要协议日期门控，而不能只相信客户端扩展声明。rmcp 当前的 `subscriptions/listen` 过滤器没有 task IDs，服务端也明确跳过 task notification 路由。

## Goals / Non-Goals

**Goals:**

- 对现代 task-capable 客户端，将真实 Focus retry ticket 提升为唯一的标准 task handle。
- 在 task 内复用现有 snapshot/replay/终态 gap 规则，不复制分析完成逻辑。
- 把 task 固定到产生 query snapshot 的项目，即使活动项目后来切换也不会在错误项目上重放。
- 提供标准轮询和协作取消，同时保持旧客户端和核心直接调用的现有行为。

**Non-Goals:**

- 不把所有工具调用预先异步化；快速终态调用仍直接完成。
- 不取消共享的 Focus 物化作业；取消只放弃该客户端 task 的等待和结果重放。
- 不删除 `query_id`、`tasks` 或 `resume_query` 兼容工具，也不改变 15 项工具目录。
- 不实现跨进程任务恢复、远程无状态传输、持久化 task store 或 `subscriptions/listen` task 推送。
- 不把 MRTR 表单/候选输入移入 task；`explore` 即使返回 Focus retry ticket，也继续使用现有 `query_id` 恢复流程。

## Decisions

### 1. 先执行现有调用，只提升真实 retry ticket

适配层继续按原路径执行 `ToolRouter::call_tool()`，保留交互预算、进度通知、并发门控和快速终态。结果满足以下全部条件时才可 taskify：

- 工具结果不是 error；
- 内容是唯一文本 JSON；
- 包含非空字符串 `query_id`；
- `analysis.retry_after_ms` 是正整数；
- 当前项目中仍存在该 query snapshot，能够创建固定项目 replay router；
- 有效协议版本不早于 `2026-07-28`，且客户端声明 Tasks Extension；
- 工具不是 `explore`，因为其终态重放仍可能需要候选选择 MRTR。

转换后客户端只看到 `CreateTaskResult`，不会看到中间 retry ticket。若任一条件不满足，返回原完整结果，不猜测可恢复性。

备选方案是在调用前把所有可能较慢的工具都变成 task；这会让大量快速查询失去同步结果，并复制 Atlas 对哪些响应真正 retryable 的判断，因此不采用。

### 2. task 捕获固定项目 replay router

原同步工具调用得到结果后、仍在同一个 blocking 执行闭包并持有其并发 permit 时，适配层立即根据 `query_id` 在当前 `ActiveProject` 的 snapshot map 中确认存在，并构造持有同一 `Arc<ActiveProject>` 的 replay router；随后才把结果和已固定的 replay 一并带回 async 层创建 task。这会关闭“释放初始调用后再读取活动项目”的切换窗口。后续活动项目切换不改变该 task 的数据边界；若无法绑定，则不创建 task，回退原 retry ticket。

TaskManager 使用与 query snapshot 相同的 300 秒 TTL。task loop 还维护独立硬截止时间，避免在没有客户端轮询触发 TaskManager 机会式清理时无限存活。`pollIntervalMs` 取 Atlas 本身已限制到 60 秒内的 retry ticket 建议值，并限制在 250..60000 毫秒。任务循环调用现有 `resume_query` 核心路径；若结果仍含同一 query ID 的 retry marker，就按新建议等待并再次重放，否则把完整 `CallToolResult` 作为 task 终态结果。服务销毁时显式调用 `TaskManager::shutdown()`，中止并清空仍在运行的 task。

### 3. 取消是 task waiter 的协作取消

任务在睡眠前后和每次重放之间检查 `TaskContext` 的取消状态。收到 `tasks/cancel` 后，任务停止等待/重放并进入 `cancelled`。已经进入 `spawn_blocking` 的一次同步重放不会被强行中断，但其结果会在返回后因取消状态而丢弃。

Focus 物化可能服务其他查询并写入共享缓存，强制中止会破坏现有 runtime 的合并与复用语义，因此本切片不取消底层共享作业。

### 4. task 终态保持 MCP 工具错误语义

现有 `CallToolResult.isError: true` 是工具级终态，按 SEP-2663 仍作为 `completed` task 的 `result` 返回。只有适配器失败，例如并发 gate 关闭或 task worker join 失败，才进入 task `failed` JSON-RPC error payload。snapshot 过期或 Focus 失败仍沿用现有核心的完整工具结果与 gap 语义。

### 5. 标准方法由一个服务级 TaskManager 承担

`AtlasMcpService` 持有共享 `TaskManager`，`get_info()` 声明 Tasks Extension；`get_task`、`update_task`、`cancel_task` 直接委托 manager。三种方法额外检查有效现代协议，避免 MCP `2025-11-25` 客户端即使伪造扩展能力也启用该新扩展。

当前 task 不产生 in-task input request，`tasks/update` 仍按 rmcp 标准处理未知或已消费 key，给后续组合留下兼容入口。

### 6. 本切片只采用轮询

客户端按 `pollIntervalMs` 调用 `tasks/get`。rmcp 3.0.1 当前明确不把 `notifications/tasks` 发送到 `subscriptions/listen`，其 `SubscriptionFilter` 也没有 task IDs；在 SDK 支持完整路由前自行建立第二套推送注册表会重新引入双控制面，因此不实现。

## Risks / Trade-offs

- [初次调用仍可能消耗完整交互预算后才返回 task] → 这是有意保持“快速查询同步完成”的兼容策略；只对确实超时的查询增加 task。
- [task ID 与内部 query ID 生命周期不同] → 对外只暴露 task ID；TTL 对齐，task closure 私有持有 query ID，不新增可查询映射。
- [活动项目切换导致错误项目重放] → 创建 task 前固定 snapshot 所属 `ActiveProject`；绑定失败则回退原结果。
- [取消后共享物化继续消耗资源] → 明确为缓存/共享工作语义；task 本身停止轮询和重放，未来若 runtime 支持引用计数取消再独立设计。
- [客户端从终态结果中看到历史 `query_id`] → 终态保持核心结果不变；它不再是同时进行中的第二句柄，且兼容工具在迁移期仍保留。
- [服务向旧协议声明未知扩展] → 实际 task 结果和 `tasks/*` 方法均执行现代协议门控；旧客户端继续收到原核心结果。

## Migration Plan

1. 加入纯 retry-ticket 识别、项目绑定 helper 与 TaskManager 服务方法测试。
2. 接入 `tools/call` 结果转换，验证 task-capable/legacy/终态/MRTR 分支与固定项目重放。
3. 运行完整 MCP/CLI 回归并观察 task TTL、取消和错误语义。
4. 如需回滚，移除适配层 task 转换和 server capability；query snapshot、兼容工具和核心结果无需迁移。
