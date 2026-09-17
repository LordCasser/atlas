## 1. Tasks 能力与识别

- [x] 1.1 加入现代协议 + 客户端 Tasks Extension 门控，并让服务器声明扩展；覆盖旧协议伪造能力、无能力和现代能力
- [x] 1.2 实现严格 retry ticket 识别，只接受非错误、唯一文本 JSON、非空 `query_id` 与正 `retry_after_ms`
- [x] 1.3 实现 query snapshot 的固定项目 replay router；绑定失败必须回退原结果

## 2. Task 生命周期集成

- [x] 2.1 在 `AtlasMcpService` 接入共享 `TaskManager`，仅把真实 retryable 结果转换为 `CreateTaskResult`
- [x] 2.2 实现固定项目上的 `resume_query` 循环、动态等待、300 秒 TTL、共享并发 gate 与工具结果终态转换
- [x] 2.3 实现并测试 `tasks/get`、`tasks/update`、`tasks/cancel`、未知 ID、旧协议拒绝和协作取消
- [x] 2.4 验证快速终态、工具错误、MRTR、legacy/no-capability、项目切换、15 项目录/schema 及核心直接结果不变

## 3. 文档与验收

- [x] 3.1 更新路线图和变更日志，明确现代客户端每个进行中调用只看到 task handle、旧客户端保留 query_id 控制面，推送订阅留待 rmcp 支持
- [x] 3.2 运行严格 OpenSpec、格式检查、`atlas-mcp` 全特性测试、MCP CLI 测试和全目标 `-D warnings` Clippy
- [x] 3.3 完成独立审查并修复有效发现，归档后运行全部 OpenSpec 严格校验与 `cargo clean`
