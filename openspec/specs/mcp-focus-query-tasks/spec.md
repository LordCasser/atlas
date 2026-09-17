# mcp-focus-query-tasks Specification

## Purpose
将现代 task-capable 客户端遇到的真实 Focus retryable 查询提升为单一 MCP task handle，同时保持 Atlas 现有 snapshot replay、严格终态、旧协议兼容和项目隔离。

## Requirements

### Requirement: Tasks Extension 必须按协议和双方能力启用

系统 SHALL 在服务器声明 `io.modelcontextprotocol/tasks`、有效协议版本不早于 `2026-07-28` 且客户端声明该扩展时启用 Focus query task。旧协议或未声明扩展的客户端 MUST 保持现有完整工具结果。

#### Scenario: 现代 task-capable 客户端

- **WHEN** 客户端协商有效 `2026-07-28+` 协议并声明 Tasks Extension
- **THEN** 系统可以为符合条件的 retryable Focus 查询返回 `resultType: "task"`

#### Scenario: 旧协议声明未知扩展

- **WHEN** 客户端协商 `2025-11-25`，即使其能力对象包含 Tasks Extension
- **THEN** 系统仍返回现有 `query_id` retry ticket，并不得创建 MCP task

#### Scenario: 现代客户端未声明扩展

- **WHEN** 客户端未声明 Tasks Extension
- **THEN** 系统返回现有同步终态或 `query_id` retry ticket，不返回 task handle

### Requirement: 只有真实且可绑定的 retry ticket 才能转换为 task

系统 MUST 仅在非错误工具结果包含唯一文本 JSON、非空 `query_id`、正整数 `analysis.retry_after_ms`，且该 query snapshot 能绑定到当前项目时创建 task。`explore` 还 MUST 要求客户端同时声明表单 elicitation；其他工具只要求 Tasks Extension。其余结果 SHALL 原样完成。

#### Scenario: 交互预算后仍在物化

- **WHEN** 现有非 `explore` 工具核心返回可绑定的 Focus retry ticket
- **THEN** task-capable 客户端只收到一个 `CreateTaskResult`，中间 `query_id` ticket 不作为本次调用结果暴露

#### Scenario: 双能力客户端的 explore 仍在物化

- **WHEN** 有效现代客户端同时声明 Tasks Extension 与表单 elicitation，且 `explore` 返回可绑定的 Focus retry ticket
- **THEN** 系统返回一个 task handle，并隐藏本次调用的中间 `query_id` ticket

#### Scenario: explore 缺少表单能力

- **WHEN** 客户端声明 Tasks Extension 但未声明表单 elicitation，且 `explore` 返回 retry ticket
- **THEN** 系统返回现有完整 `query_id` retry ticket，不创建 task

#### Scenario: 快速终态不创建 task

- **WHEN** 工具在交互预算内返回终态成功、工具错误或 `explore` 候选 MRTR
- **THEN** 系统直接返回现有完整 `CallToolResult` 或 `InputRequiredResult`

#### Scenario: 伪造或无法绑定的 marker 不创建 task

- **WHEN** 结果缺少有效 query marker、带有 error、snapshot 已消失，或 snapshot 不属于当前可固定项目
- **THEN** 系统返回原完整结果，不创建或猜测 task

#### Scenario: MRTR 结果保持独立

- **WHEN** `explore` 快速返回候选 `input_required`，或 `project(open)` 返回 `input_required`
- **THEN** 系统保留原直接 MRTR 流程，不把该输入请求包装为 task

### Requirement: task 必须在原项目复用现有 replay 和终态规则

系统 MUST 将 task 固定到产生 query snapshot 的 `ActiveProject`，并通过现有 `resume_query` 路径重放，直到返回不再 retryable 的完整 `CallToolResult`。活动项目切换 MUST NOT 改变 task 的项目边界。对于 task 内接受候选后产生的新 retry ticket，系统 MUST 先确认新 snapshot 属于同一固定项目，且 SHALL 在同一 task 和原硬截止时间内继续，不得创建嵌套 task。

#### Scenario: task 完成原查询

- **WHEN** Focus 物化完成且客户端轮询 `tasks/get`
- **THEN** task 最终为 `completed`，`result` 包含现有重放得到的完整工具结果

#### Scenario: task 保留终态 gap

- **WHEN** Focus 作业失败、snapshot 过期或重放得到现有工具级终态错误
- **THEN** task 以 `completed` 返回该 `isError: true` 工具结果，而不是伪造 JSON-RPC 成功数据或重新发布部分语义结果

#### Scenario: 初始调用与项目切换并发

- **WHEN** 一个可能转换为 task 的项目作用域调用与 `project(open)` 并发
- **THEN** 系统在进入初始核心调用前固定一个项目身份，并在该同一项目执行工具、写入 snapshot 和准备 task，不得混合两个项目的状态

#### Scenario: 项目在 task 期间切换

- **WHEN** task 创建后客户端打开另一个项目
- **THEN** 后续重放和接受候选后的精确调用仍读取原 snapshot 所属项目，不访问新项目的 snapshot 或数据库

#### Scenario: 接受候选后产生新的内部 retry

- **WHEN** task 接受精确候选后的 `explore` 调用返回同一固定项目中的新 retry ticket
- **THEN** 系统在原 task 内采用该内部 query snapshot 继续等待，并不得向客户端返回第二个 task ID 或进行中的 query ID

### Requirement: task 生命周期必须可轮询、更新和取消

系统 SHALL 实现标准 `tasks/get`、`tasks/update` 和 `tasks/cancel`，task TTL MUST 与 query snapshot 的 300 秒 TTL 对齐，task 自身 MUST 在没有客户端轮询时也执行该截止时间，并 SHALL 提供有界正数 `pollIntervalMs`。服务关闭时 MUST 停止并清除仍在运行的 task。

#### Scenario: 轮询工作中 task

- **WHEN** 客户端在 Focus 仍未完成时调用 `tasks/get`
- **THEN** 系统返回 `working` 和 task 元数据，不返回内部 retry ticket

#### Scenario: 取消 task

- **WHEN** 客户端调用 `tasks/cancel`
- **THEN** 系统确认取消请求，task 协作停止等待和重放并最终进入 `cancelled`
- **AND** 共享 Focus 物化 MAY 继续完成并写入缓存

#### Scenario: task 在无轮询时过期

- **WHEN** 客户端创建 task 后不再调用任何 task 方法，且 Focus 在 300 秒内仍未完成
- **THEN** task 自身停止重放并进入 `failed`，不得依赖下一次客户端轮询才停止工作

#### Scenario: 服务关闭

- **WHEN** MCP 服务在 task 运行期间关闭
- **THEN** 系统中止并清除该服务管理的运行中 task

#### Scenario: 未知 task

- **WHEN** 客户端读取、更新或取消未知/过期 task ID
- **THEN** 系统返回标准 `invalid_params`

#### Scenario: 旧协议调用 task 方法

- **WHEN** `2025-11-25` 客户端调用 `tasks/get`、`tasks/update` 或 `tasks/cancel`
- **THEN** 系统不启用该扩展方法，不泄露 task 状态

### Requirement: 兼容工具与核心契约必须保持不变

系统 MUST 保持 15 项工具目录、所有 tool schema、`ToolRouter::call_tool()` 直接结果以及旧客户端的 `query_id` / `tasks` / `resume_query` 行为不变。一个 task-capable 进行中调用 MUST NOT 同时向客户端返回 task ID 与 retry ticket。

#### Scenario: 旧客户端继续手动恢复

- **WHEN** 未声明 Tasks Extension 的客户端收到 retry ticket
- **THEN** 它可以继续使用现有 `tasks(query_id)` 和 `resume_query(query_id)` 流程

#### Scenario: 目录与 schema 不变

- **WHEN** 比较启用 Tasks Extension 前后的 `tools/list`
- **THEN** 工具数量、顺序、名称和 input schema 完全一致

### Requirement: explore task 必须支持一次有界候选输入

当 task-capable `explore` 重放首次得到有效多候选结果时，系统 SHALL 通过 task `inputRequests` 发布一个表单 elicitation，并 MUST 等待对应 `tasks/update.inputResponses` 后再继续。每个 task MUST 至多发布一次候选输入请求。

#### Scenario: task 从 working 进入 input_required

- **WHEN** `explore` task 的 Focus 重放得到现有 `ambiguous: true` 且至少两个有效候选的结果
- **THEN** `tasks/get` 返回 `input_required`，并内联唯一候选 `inputRequests`

#### Scenario: 输入期间取消 task

- **WHEN** task 处于 `input_required` 且客户端调用 `tasks/cancel`
- **THEN** pending input 被清除，task 不执行候选后的调用并最终进入 `cancelled`

#### Scenario: 输入期间达到硬截止

- **WHEN** task 在 300 秒硬截止前未收到候选响应
- **THEN** task 清除 pending input、停止重放并进入 `failed`

#### Scenario: task 不形成候选输入循环

- **WHEN** 已消费一次候选响应的 task 后续再次得到多候选结果
- **THEN** 系统以该完整候选工具结果完成 task，不再发布第二个 input request
