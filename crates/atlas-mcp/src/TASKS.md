# Task/Wait 架构约束

## TaskManager

- TaskManager 是 MCP server 进程内的纯内存任务表，不做持久化。
- 自动任务 ID 必须使用 `task-xxxxx` 十六进制递增格式；普通 `background: true` 工具只能使用自动 ID。
- 自定义任务 ID 只能用于需要跨存储聚合状态的稳定任务域，例如 `analysis:{short_hash}`；重复 ID 创建必须失败。
- 每个任务记录 `task_id`、`tool_name`、`method`、`status`、`progress`、`result`、`error`、`created_at`、`completed_at`。
- 生命周期只允许 `Running -> Completed(result)` 或 `Running -> Failed(error)`。
- `progress` 表示 0.0 到 100.0 的百分比；运行中进度更新按 5 个百分点节流，初始更新和 100% 更新必须记录。
- `create_task`、`create_task_with_id` 和 `get_task` 都必须触发自动剪枝：Running 永远保留；Completed/Failed 保留 300 秒。

## Wait/Status

- `task_status` 是单次查询，不阻塞。
- `wait_for_task` 是 server 端主动轮询；`timeout_secs=0` 表示单次查询；最大等待时间限制为 300 秒。
- 等待机制不依赖 MCP 协议层 progress notification；后台任务进度以内存 TaskManager 为准。
